# @magicblock-labs/wheels

Define a fixed binary layout and use it to encode and decode Node.js buffers.

```ts
import { makeLayout, ty } from "@magicblock-labs/wheels";

const layout = makeLayout([ty.u8("version"), ty.u64("amount")]);
const bytes = layout.encode({ version: 1, amount: 42n });
const record = layout.decode(bytes);
```

The package includes TypeScript declarations. Numeric fields are little-endian.
Use `ty.pubkey` for a `PublicKey` from `@solana/web3.js` v1.

Run `npm ci` and `npm test` in this directory to build the package and test its tarball.
