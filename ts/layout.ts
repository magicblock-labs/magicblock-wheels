import { PublicKey } from "@solana/web3.js";
import { z } from "zod";

type Kind = "u8" | "u64" | "i64" | "pubkey";

export interface Field<Name extends string, K extends Kind> {
  name: Name;
  kind: K;
}

export const ty = {
  u8<Name extends string>(name: Name) {
    return { name, kind: "u8" } as const satisfies Field<Name, "u8">;
  },

  u64<Name extends string>(name: Name) {
    return { name, kind: "u64" } as const satisfies Field<Name, "u64">;
  },

  i64<Name extends string>(name: Name) {
    return { name, kind: "i64" } as const satisfies Field<Name, "i64">;
  },

  pubkey<Name extends string>(name: Name) {
    return { name, kind: "pubkey" } as const satisfies Field<Name, "pubkey">;
  },

  skip<N extends number>(n: N) {
    return { kind: "skip", size: n } as const satisfies NoField;
  },

  padding<N extends number>(n: N) {
    return { kind: "padding", size: n } as const satisfies NoField;
  },
};

type NoField = {
  kind: "skip" | "padding",
  size: number,
};

type LayoutItem = Field<string, Kind> | NoField;

type KindToTs<K extends Kind> =
  K extends "u8" ? number :
  K extends "u64" ? bigint :
  K extends "i64" ? bigint :
  K extends "pubkey" ? PublicKey :
  never;

export type FieldsToTs<Defs extends readonly LayoutItem[]> = {
  [F in Defs[number]as F extends { name: string } ? F["name"] : never]:
  F extends Field<infer Name, infer K>
  ? KindToTs<K>
  : never;
};


export function makeLayout<const Defs extends readonly LayoutItem[]>(
  defs: Defs
) {
  const [schema, space] = function() {
    let space = 0;
    const shape: Record<string, z.ZodTypeAny> = {};
    for (const f of defs) {
      if (f.kind === "u8") {
        space += 1;
        shape[f.name] = z.number();
      } else if (f.kind === "u64" || f.kind == "i64") {
        space += 8;
        shape[f.name] = z.bigint();
      } else if (f.kind === "pubkey") {
        space += 32;
        shape[f.name] = z.instanceof(PublicKey);
      } else if (f.kind == "skip" || f.kind == "padding") {
        space += f.size;
      } else {
        throw Error(`Unsupported layout kind: ${f.kind}`);
      }
    }
    return [z.object(shape), space] as const;
  }();

  function decode(buf: Buffer): FieldsToTs<Defs> {
    let offset = 0;
    const out: any = {};

    if (buf.length != space) {
      throw Error(`Error in decode: expecting buffer of size ${space}, got ${buf.length}`);
    }

    for (const f of defs) {
      try {
        if (f.kind === "u8") {
          out[f.name] = buf.readUInt8(offset);
          offset += 1;
        } else if (f.kind === "u64") {
          out[f.name] = buf.readBigUInt64LE(offset);
          offset += 8;
        } else if (f.kind === "i64") {
          out[f.name] = buf.readBigInt64LE(offset);
          offset += 8;
        } else if (f.kind === "pubkey") {
          out[f.name] = new PublicKey(buf.slice(offset, offset + 32));
          offset += 32;
        } else if (f.kind === "skip" || f.kind == "padding") {
          offset += f.size;
        } else {
          throw Error(`Unsupported layout kind: ${f.kind}`);
        }
      } catch (e) {
        console.error(`Error in decode while parsing ${JSON.stringify(f)}: ${JSON.stringify(e)}`);
        throw e;
      }
    }
    return schema.parse(out) as FieldsToTs<Defs>;
  }

  function encode(value: FieldsToTs<Defs>): Buffer {
    let offset = 0;
    let out: Buffer = Buffer.alloc(space);
    const val = value as any;

    for (const f of defs) {
      if (f.kind === "u8") {
        out.writeUInt8(val[f.name], offset);
        offset += 1;
      } else if (f.kind === "u64") {
        out.writeBigUInt64LE(val[f.name], offset);
        offset += 8;
      } else if (f.kind === "i64") {
        out.writeBigInt64LE(val[f.name], offset);
        offset += 8;
      } else if (f.kind === "pubkey") {
        Buffer.from(val[f.name].toBytes()).copy(out, offset);
        offset += 32;
      } else if (f.kind === "padding") {
        out.fill(0, offset, offset + f.size);
        offset += f.size;
      } else if (f.kind == "skip") {
        throw Error(`layout with skipped fields cannot be encoded`);
      } else {
        throw Error(`Unsupported layout kind: ${f.kind}`);
      }
    }
    return out;
  }

  return { defs, schema, decode, encode, space };
}

export type TypeOf<L> = L extends { decode: (buf: Buffer) => infer R } ? R : never;
