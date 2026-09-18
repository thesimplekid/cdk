import {
  Amount,
  splitAmount,
  type AmountLike,
  type HasKeysetKeys,
  type OutputDataCreator,
  type OutputDataLike,
  type P2PKOptions,
  type Proof,
} from "@cashu/cashu-ts";
import type * as Native from "./generated/cashu_ffi";

/** Synchronous native boundary; injectable for host-side conformance testing. */
export type NativeCrypto = Pick<
  typeof Native,
  | "createRandomOutput"
  | "createDeterministicOutput"
  | "createRandomOutputs"
  | "createDeterministicOutputs"
  | "createRestoreOutputs"
  | "createLockedOutputs"
  | "normalizePublicKeys"
  | "unblindOutput"
>;

const MAX_U64 = (1n << 64n) - 1n;
const RESERVED_TAGS = new Set([
  "locktime",
  "pubkeys",
  "n_sigs",
  "refund",
  "n_sigs_refund",
  "sigflag",
]);

function amountValue(value: AmountLike): bigint {
  const amount = BigInt(Amount.from(value).toString());
  if (amount < 0n || amount > MAX_U64)
    throw new RangeError("Amount exceeds the native u64 range");
  return amount;
}

function counterValue(counter: number): bigint {
  if (!Number.isSafeInteger(counter) || counter < 0)
    throw new RangeError("Counter must be a nonnegative safe integer");
  return BigInt(counter);
}

/** Adapt the synchronous Rust API to the cashu-ts 4.10.2 output strategy. */
export class RustOutputDataCreator implements OutputDataCreator {
  constructor(private readonly crypto: NativeCrypto) {}

  private wrap(output: Native.NativeOutput): OutputDataLike {
    const native = this.crypto;
    const result: OutputDataLike = {
      blindedMessage: {
        amount: Amount.from(output.amount),
        id: output.keysetId,
        B_: output.blindedMessage,
      },
      blindingFactor: BigInt(`0x${output.blindingFactor}`),
      secret: new Uint8Array(output.secret),
      ...(output.ephemeralE ? { ephemeralE: output.ephemeralE } : {}),
      toProof(signature, keyset): Proof {
        if (keyset.id !== result.blindedMessage.id)
          throw new Error("Keyset does not match output");
        const signedAmount = amountValue(signature.amount);
        const mintKey = keyset.keys[signedAmount.toString()];
        if (!mintKey) throw new Error("Missing mint key for signature amount");
        // Read current output fields: cashu-ts can assign amounts to blank outputs.
        const c = native.unblindOutput(
          {
            amount: amountValue(result.blindedMessage.amount),
            keysetId: result.blindedMessage.id,
            blindedMessage: result.blindedMessage.B_,
            secret: result.secret.slice().buffer,
            blindingFactor: result.blindingFactor
              .toString(16)
              .padStart(64, "0"),
            ephemeralE: result.ephemeralE,
          },
          {
            amount: signedAmount,
            keysetId: signature.id,
            blindedSignature: signature.C_,
            dleq: signature.dleq,
          },
          mintKey,
        );
        return {
          amount: Amount.from(signedAmount),
          id: signature.id,
          C: c,
          secret: new TextDecoder("utf-8", { fatal: true }).decode(
            result.secret,
          ),
          ...(signature.dleq
            ? {
                dleq: {
                  ...signature.dleq,
                  r: result.blindingFactor.toString(16).padStart(64, "0"),
                },
              }
            : {}),
          ...(result.ephemeralE ? { p2pk_e: result.ephemeralE } : {}),
        };
      },
    };
    return result;
  }

  createRandomData(
    amount: AmountLike,
    keyset: HasKeysetKeys,
    customSplit?: AmountLike[],
  ): OutputDataLike[] {
    return this.crypto
      .createRandomOutputs(
        splitAmount(amount, keyset.keys, customSplit).map(amountValue),
        keyset.id,
      )
      .map((output) => this.wrap(output));
  }

  createSingleRandomData(amount: AmountLike, keysetId: string): OutputDataLike {
    return this.wrap(
      this.crypto.createRandomOutput(amountValue(amount), keysetId),
    );
  }

  createDeterministicData(
    amount: AmountLike,
    seed: Uint8Array,
    counter: number,
    keyset: HasKeysetKeys,
    customSplit?: AmountLike[],
  ): OutputDataLike[] {
    return this.crypto
      .createDeterministicOutputs(
        splitAmount(amount, keyset.keys, customSplit).map(amountValue),
        seed.slice().buffer,
        counterValue(counter),
        keyset.id,
      )
      .map((output) => this.wrap(output));
  }

  createSingleDeterministicData(
    amount: AmountLike,
    seed: Uint8Array,
    counter: number,
    keysetId: string,
  ): OutputDataLike {
    return this.wrap(
      this.crypto.createDeterministicOutput(
        amountValue(amount),
        seed.slice().buffer,
        counterValue(counter),
        keysetId,
      ),
    );
  }

  /** Blank restore outputs in one native call; chunk scans above 10,000 counters. */
  createRestoreData(
    seed: Uint8Array,
    keysetId: string,
    counter: number,
    count: number,
  ): OutputDataLike[] {
    if (!Number.isSafeInteger(count) || count < 0 || count > 10_000)
      throw new RangeError("Invalid restore batch size");
    return this.crypto
      .createRestoreOutputs(
        seed.slice().buffer,
        keysetId,
        counterValue(counter),
        count,
      )
      .map((output) => this.wrap(output));
  }

  private normalize(options: P2PKOptions): P2PKOptions {
    const dedupe = (keys: string[]) => {
      const seen = new Set<string>();
      return this.crypto.normalizePublicKeys(keys).filter((key) => {
        const x = key.slice(2);
        if (seen.has(x)) return false;
        seen.add(x);
        return true;
      });
    };
    const main = dedupe(
      Array.isArray(options.pubkey) ? options.pubkey : [options.pubkey],
    );
    const refund = dedupe(options.refundKeys ?? []);
    const htlc = !!options.hashlock;
    if (!main.length && !htlc) throw new Error("P2PK requires a public key");
    if (main.length + refund.length + Number(htlc) > 11)
      throw new Error("Too many locking keys");
    if (
      options.sigFlag !== undefined &&
      options.sigFlag !== "SIG_ALL" &&
      options.sigFlag !== "SIG_INPUTS"
    )
      throw new Error("Invalid signature flag");
    for (const [threshold, count] of [
      [options.requiredSignatures, main.length],
      [options.requiredRefundSignatures, refund.length],
    ] as const) {
      if (
        threshold !== undefined &&
        (!Number.isSafeInteger(threshold) || threshold < 1 || threshold > count)
      )
        throw new Error("Invalid signature threshold");
    }
    if (refund.length && options.locktime === undefined)
      throw new Error("Refund keys require a locktime");
    return { ...options, pubkey: main, refundKeys: refund };
  }

  createP2PKData(
    options: P2PKOptions,
    amount: AmountLike,
    keyset: HasKeysetKeys,
    customSplit?: AmountLike[],
  ): OutputDataLike[] {
    const amounts = splitAmount(amount, keyset.keys, customSplit).map(
      amountValue,
    );
    if (!amounts.length) return [];
    return this.crypto
      .createLockedOutputs(
        amounts,
        keyset.id,
        this.lockingData(options),
        undefined,
      )
      .map((output) => this.wrap(output));
  }

  createSingleP2PKData(
    options: P2PKOptions,
    amount: AmountLike,
    keysetId: string,
    eBytes?: Uint8Array,
  ): OutputDataLike {
    return this.wrap(
      this.crypto.createLockedOutputs(
        [amountValue(amount)],
        keysetId,
        this.lockingData(options),
        eBytes?.slice().buffer,
      )[0],
    );
  }

  private lockingData(options: P2PKOptions): Native.NativeLock {
    const normalized = this.normalize(options);
    const main = Array.isArray(normalized.pubkey)
      ? normalized.pubkey
      : [normalized.pubkey];
    const refund = normalized.refundKeys ?? [];
    const htlc = !!normalized.hashlock;
    const data = htlc ? normalized.hashlock! : main[0];
    const pubkeys = htlc ? main : main.slice(1);
    const tags: string[][] = [];
    if (normalized.locktime !== undefined) {
      if (!Number.isSafeInteger(normalized.locktime) || normalized.locktime < 0)
        throw new RangeError("Invalid locktime");
      tags.push(["locktime", String(normalized.locktime)]);
    }
    if (pubkeys.length) {
      tags.push(["pubkeys", ...pubkeys]);
      if ((normalized.requiredSignatures ?? 1) > 1)
        tags.push(["n_sigs", String(normalized.requiredSignatures)]);
    }
    if (refund.length) {
      tags.push(["refund", ...refund]);
      if ((normalized.requiredRefundSignatures ?? 1) > 1)
        tags.push([
          "n_sigs_refund",
          String(normalized.requiredRefundSignatures),
        ]);
    }
    if (normalized.sigFlag === "SIG_ALL") tags.push(["sigflag", "SIG_ALL"]);
    for (const [key, ...values] of normalized.additionalTags ?? []) {
      if (!key || RESERVED_TAGS.has(key))
        throw new Error("Invalid or reserved additional tag");
      tags.push([key, ...values.map(String)]);
    }
    return { data, tags, htlc, blindKeys: !!normalized.blindKeys };
  }
}
