import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { test } from "node:test";
import {
  Amount,
  OutputData,
  Wallet,
  createBlindSignature,
  createDLEQProof,
  getPubKeyFromPrivKey,
  pointFromHex,
  type HasKeysetKeys,
  type OutputDataLike,
  type P2PKOptions,
  type SerializedBlindedSignature,
} from "@cashu/cashu-ts";
import { RustOutputDataCreator, type NativeCrypto } from "../src/creator";

const binary = process.env.CDK_CRYPTO_TEST_BIN;
if (!binary)
  throw new Error("Set CDK_CRYPTO_TEST_BIN to the built conformance example");
function call(method: string, args: unknown[] = []): any {
  // JSON.rawJSON preserves u64 amounts/counters in the test-only transport.
  const input = JSON.stringify(
    {
      method,
      args:
        method === "randomSecretKey"
          ? undefined
          : method === "normalizePublicKeys"
            ? args[0]
            : args,
    },
    (_key, value) =>
      typeof value === "bigint"
        ? (JSON as any).rawJSON(value.toString())
        : value instanceof ArrayBuffer
          ? Array.from(new Uint8Array(value))
          : value,
  );
  const result = JSON.parse(
    execFileSync(binary!, {
      input,
      encoding: "utf8",
      stdio: ["pipe", "pipe", "pipe"],
    }),
  );
  if (method === "randomSecretKey") return Uint8Array.from(result).buffer;
  if (method.startsWith("create")) {
    const outputs = Array.isArray(result) ? result : [result];
    for (const [i, output] of outputs.entries()) {
      // Restore amounts are zero; other amounts originate in the request to preserve u64.
      output.amount =
        method === "createRestoreOutputs"
          ? 0n
          : Array.isArray(result)
            ? (args[0] as bigint[])[i]
            : args[0];
      output.secret = Uint8Array.from(output.secret).buffer;
      output.ephemeralE ??= undefined;
    }
  }
  if (method === "blindLockingKeys") result.ephemeralE ??= undefined;
  return result;
}
const native = Object.fromEntries(
  [
    "createRandomOutput",
    "createDeterministicOutput",
    "createLockedOutput",
    "createRandomOutputs",
    "createDeterministicOutputs",
    "createRestoreOutputs",
    "createLockedOutputs",
    "randomSecretKey",
    "normalizePublicKeys",
    "blindLockingKeys",
    "unblindOutput",
  ].map((name) => [name, (...args: unknown[]) => call(name, args)]),
) as NativeCrypto;
const creator = new RustOutputDataCreator(native);
const id = "009a1f293253e41e";
const mintSecret = new Uint8Array(32).fill(1);
const mintKey = Buffer.from(getPubKeyFromPrivKey(mintSecret)).toString("hex");
const otherKey = Buffer.from(
  getPubKeyFromPrivKey(new Uint8Array(32).fill(2)),
).toString("hex");
const keyset: HasKeysetKeys = {
  id,
  keys: Object.fromEntries(["1", "2", "4", "8", "16"].map((a) => [a, mintKey])),
};
function signature(
  output: OutputDataLike,
  amount = output.blindedMessage.amount,
): SerializedBlindedSignature {
  const b = pointFromHex(output.blindedMessage.B_);
  const sig = createBlindSignature(b, mintSecret, id);
  const dleq = createDLEQProof(b, mintSecret);
  return {
    amount,
    id,
    C_: sig.C_.toHex(true),
    dleq: {
      e: Buffer.from(dleq.e).toString("hex"),
      s: Buffer.from(dleq.s).toString("hex"),
    },
  };
}
function compareProof(output: OutputDataLike) {
  const noble = new OutputData(
    output.blindedMessage,
    output.blindingFactor,
    output.secret,
    output.ephemeralE,
  );
  const sig = signature(output);
  assert.deepEqual(output.toProof(sig, keyset), noble.toProof(sig, keyset));
}

test("can be injected into the real Wallet", () => {
  assert.ok(new Wallet("https://mint.example", { outputDataCreator: creator }));
});

test("NUT-13 matches cashu-ts for both keyset versions, seed lengths and counter boundaries", () => {
  for (const keysetId of [id, `01${"ab".repeat(32)}`]) {
    for (const length of [16, 32, 64]) {
      for (const counter of [
        0,
        12,
        2 ** 31 - 1,
        ...(keysetId.startsWith("01")
          ? [2 ** 32, Number.MAX_SAFE_INTEGER]
          : []),
      ]) {
        const seed = new Uint8Array(length).fill(42);
        const actual = creator.createSingleDeterministicData(
          8,
          seed,
          counter,
          keysetId,
        );
        const expected = OutputData.createSingleDeterministicData(
          8,
          seed,
          counter,
          keysetId,
        );
        assert.deepEqual(
          OutputData.serialize(actual),
          OutputData.serialize(expected),
        );
      }
    }
  }
});

test("batch splitting and counters match cashu-ts", () => {
  const seed = new Uint8Array(64).fill(3);
  assert.deepEqual(
    creator
      .createDeterministicData(15, seed, 20, keyset, [1, 2, 4, 8])
      .map(OutputData.serialize),
    OutputData.createDeterministicData(15, seed, 20, keyset, [1, 2, 4, 8]).map(
      OutputData.serialize,
    ),
  );
});

test("random outputs unblind and verify DLEQ exactly like cashu-ts", () => {
  const outputs = creator.createRandomData(15, keyset);
  assert.equal(
    Amount.sum(outputs.map((o) => o.blindedMessage.amount)).toString(),
    "15",
  );
  assert.equal(
    new Set(outputs.map((o) => Buffer.from(o.secret).toString("hex"))).size,
    outputs.length,
  );
  outputs.forEach(compareProof);
});

test("P2PK, HTLC and P2BK preserve locking data and unblind in Rust", () => {
  for (const options of [
    { pubkey: mintKey },
    {
      pubkey: [mintKey, otherKey],
      requiredSignatures: 2,
      additionalTags: [["custom", "value"]],
    },
    { pubkey: mintKey, refundKeys: [otherKey], locktime: 2000000000 },
    { pubkey: mintKey, blindKeys: true },
    { pubkey: [mintKey, otherKey], hashlock: "ab".repeat(32), blindKeys: true },
    { pubkey: [], hashlock: "ab".repeat(32) },
  ] satisfies P2PKOptions[]) {
    const e = new Uint8Array(32).fill(5);
    const output = creator.createSingleP2PKData(options, 8, id, e);
    const expected = OutputData.createSingleP2PKData(options, 8, id, e);
    const actualSecret = JSON.parse(new TextDecoder().decode(output.secret));
    const expectedSecret = JSON.parse(
      new TextDecoder().decode(expected.secret),
    );
    delete actualSecret[1].nonce;
    delete expectedSecret[1].nonce;
    assert.deepEqual(actualSecret, expectedSecret);
    assert.equal(output.ephemeralE, expected.ephemeralE);
    compareProof(output);
  }
});

test("SIG_ALL P2BK batch shares one ephemeral key and locking data", () => {
  const outputs = creator.createP2PKData(
    { pubkey: mintKey, blindKeys: true, sigFlag: "SIG_ALL" },
    7,
    keyset,
  );
  assert.equal(outputs.length, 3);
  assert.equal(new Set(outputs.map((o) => o.ephemeralE)).size, 1);
  assert.ok(outputs[0].ephemeralE);
  assert.equal(
    new Set(
      outputs.map(
        (o) => JSON.parse(new TextDecoder().decode(o.secret))[1].data,
      ),
    ).size,
    1,
  );
});

test("rejects altered responses, missing keys and corrupted outputs", () => {
  const output = creator.createSingleRandomData(8, id);
  const sig = signature(output);
  assert.throws(() =>
    output.toProof({ ...sig, amount: Amount.from(4) }, keyset),
  );
  assert.throws(() =>
    output.toProof({ ...sig, id: "0011111111111111" }, keyset),
  );
  assert.throws(() =>
    output.toProof(
      { ...sig, dleq: { ...sig.dleq!, s: "01".repeat(32) } },
      keyset,
    ),
  );
  assert.throws(() => output.toProof(sig, { id, keys: {} }));
  assert.throws(() =>
    output.toProof(sig, { ...keyset, id: "0011111111111111" }),
  );
  output.secret[0] ^= 1;
  assert.throws(() => output.toProof(sig, keyset));
});

test("blank change outputs and responses without DLEQ work", () => {
  const output = creator.createSingleRandomData(0, id);
  const sig = signature(output, Amount.from(8));
  const noble = new OutputData(
    output.blindedMessage,
    output.blindingFactor,
    output.secret,
  );
  assert.deepEqual(output.toProof(sig, keyset), noble.toProof(sig, keyset));
  const noDleq = { ...sig, dleq: undefined };
  assert.deepEqual(
    output.toProof(noDleq, keyset),
    noble.toProof(noDleq, keyset),
  );
});

test("large amounts remain exact and invalid boundary inputs fail", () => {
  const amount = 1n << 63n;
  assert.equal(
    creator.createSingleRandomData(amount, id).blindedMessage.amount.toString(),
    amount.toString(),
  );
  assert.throws(() => creator.createSingleRandomData(1n << 64n, id));
  assert.throws(() =>
    creator.createSingleDeterministicData(1, new Uint8Array(15), 0, id),
  );
  assert.throws(() =>
    creator.createSingleDeterministicData(1, new Uint8Array(32), -1, id),
  );
  assert.throws(() =>
    creator.createSingleDeterministicData(1, new Uint8Array(32), 2 ** 31, id),
  );
  assert.throws(() =>
    creator.createSingleRandomData(1, `02${"ab".repeat(32)}`),
  );
  assert.throws(() =>
    creator.createSingleP2PKData(
      { pubkey: mintKey, requiredSignatures: 2 },
      1,
      id,
    ),
  );
  assert.throws(() =>
    creator.createSingleP2PKData(
      { pubkey: mintKey, additionalTags: [["pubkeys", otherKey]] },
      1,
      id,
    ),
  );
});

test("multi-output creation crosses the output boundary once per batch", () => {
  const calls: string[] = [];
  const counted = new RustOutputDataCreator(
    new Proxy(native, {
      get(target, property: keyof NativeCrypto) {
        return (...args: unknown[]) => {
          calls.push(property);
          return (target[property] as (...args: unknown[]) => unknown)(...args);
        };
      },
    }),
  );
  counted.createRandomData(15, keyset);
  assert.deepEqual(calls.splice(0), ["createRandomOutputs"]);
  counted.createDeterministicData(15, new Uint8Array(32).fill(3), 20, keyset);
  assert.deepEqual(calls.splice(0), ["createDeterministicOutputs"]);
  counted.createP2PKData({ pubkey: mintKey, blindKeys: true }, 15, keyset);
  assert.deepEqual(calls.splice(0), [
    "normalizePublicKeys",
    "normalizePublicKeys",
    "createLockedOutputs",
  ]);
  counted.createRestoreData(new Uint8Array(32).fill(3), id, 20, 4);
  assert.deepEqual(calls, ["createRestoreOutputs"]);
});

test("batch derivation and restore preserve both keyset versions and counter limits", () => {
  for (const keysetId of [id, `01${"ab".repeat(32)}`]) {
    const limit = keysetId.startsWith("00")
      ? 2 ** 31 - 1
      : Number.MAX_SAFE_INTEGER;
    for (const length of [16, 32, 64]) {
      const backing = new Uint8Array(length + 8).fill(9);
      const seed = backing.subarray(4, length + 4);
      const local = { ...keyset, id: keysetId };
      const batch = creator.createDeterministicData(
        7,
        seed,
        limit - 2,
        local,
        [4, 1, 2],
      );
      assert.deepEqual(
        batch.map(OutputData.serialize),
        OutputData.createDeterministicData(
          7,
          seed,
          limit - 2,
          local,
          [4, 1, 2],
        ).map(OutputData.serialize),
      );
      const restore = creator.createRestoreData(seed, keysetId, limit - 2, 3);
      assert.deepEqual(
        restore.map(OutputData.serialize),
        [0, 1, 2].map((i) =>
          OutputData.serialize(
            OutputData.createSingleDeterministicData(
              0,
              seed,
              limit - 2 + i,
              keysetId,
            ),
          ),
        ),
      );
      assert.throws(() =>
        creator.createDeterministicData(3, seed, limit, local),
      );
      assert.throws(() => creator.createRestoreData(seed, keysetId, limit, 2));
      assert.deepEqual(creator.createRestoreData(seed, keysetId, 0, 0), []);
    }
  }
  for (const count of [-1, 1.5, 10_001, NaN])
    assert.throws(() =>
      creator.createRestoreData(new Uint8Array(32), id, 0, count),
    );
});

test("SIG_INPUTS batches use independent blinded keys and retain refund and HTLC tags", () => {
  for (const hashlock of [undefined, "ab".repeat(32)]) {
    const outputs = creator.createP2PKData(
      {
        pubkey: [mintKey, otherKey],
        refundKeys: [otherKey],
        locktime: 123456,
        blindKeys: true,
        hashlock,
        additionalTags: [["custom", "value"]],
      },
      7,
      keyset,
    );
    assert.equal(
      new Set(outputs.map((o) => o.ephemeralE)).size,
      outputs.length,
    );
    for (const output of outputs) {
      const [kind, secret] = JSON.parse(
        new TextDecoder().decode(output.secret),
      );
      assert.equal(kind, hashlock ? "HTLC" : "P2PK");
      if (hashlock) assert.equal(secret.data, hashlock);
      assert.ok(
        secret.tags.some((t: string[]) => t[0] === "refund" && t.length === 2),
      );
      assert.ok(
        secret.tags.some(
          (t: string[]) => t[0] === "custom" && t[1] === "value",
        ),
      );
      compareProof(output);
    }
  }
});
