import assert from "node:assert/strict";
import { PublicKey } from "@solana/web3.js";
import { makeLayout, ty, type TypeOf } from "@magicblock-labs/wheels";

const layout = makeLayout([ty.u8("version"), ty.padding(1), ty.u64("amount"), ty.pubkey("owner")]);
const record: TypeOf<typeof layout> = { version: 1, amount: 513n, owner: PublicKey.default };
const bytes = layout.encode(record);
assert.equal(bytes.length, 42);
assert.deepEqual([...bytes.subarray(0, 4)], [1, 0, 1, 2]);
assert.deepEqual(layout.decode(bytes), record);
