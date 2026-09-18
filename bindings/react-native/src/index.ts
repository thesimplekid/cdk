import * as nativeCrypto from "./native";
import { RustOutputDataCreator } from "./creator";

export { RustOutputDataCreator } from "./creator";

/** Create a cashu-ts output strategy backed by the installed native Rust module. */
export function createOutputDataCreator(): RustOutputDataCreator {
  return new RustOutputDataCreator(nativeCrypto);
}
