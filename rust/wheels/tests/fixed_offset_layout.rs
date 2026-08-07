extern crate alloc;

use pinocchio::Address;
use wheels::{
    fixed_offset_layout,
    layout::{Decodable, Encodable, PrefixDecodable},
    DataLayoutError, Pubkey,
};

#[repr(align(8))]
struct Aligned<const N: usize>([u8; N]);

#[fixed_offset_layout]
struct PrivateTransferFixedArgs {
    shuttle_id: u32,
    amount: u64,
    validator: Option<[u8; 32]>,
    #[capacity = 72]
    encrypted_destination: Vec<u8>,
    checksum: u16,
}

#[test]
fn fixed_offset_layout_reserves_constant_space() {
    assert_eq!(PrivateTransferFixedArgs::DATA_LEN, 120);
    assert_eq!(PrivateTransferFixedArgs::OFFSETS, [0, 4, 12, 45, 118]);

    let value = PrivateTransferFixedArgs {
        shuttle_id: 100,
        amount: 200,
        validator: Some([1; 32]),
        encrypted_destination: vec![1, 2, 3, 4],
        checksum: 0xBEEF,
    };

    let mut aligned = Aligned([0; PrivateTransferFixedArgs::DATA_LEN]);
    let bytes = &mut aligned.0;

    bytes[0..4].copy_from_slice(&100_u32.to_le_bytes());
    bytes[4..12].copy_from_slice(&200_u64.to_le_bytes());
    bytes[12] = 1;
    bytes[13..45].copy_from_slice(&[1; 32]);
    bytes[45] = 4;
    bytes[46..50].copy_from_slice(&[1, 2, 3, 4]);
    bytes[118..120].copy_from_slice(&0xBEEF_u16.to_le_bytes());

    let view = PrivateTransferFixedArgs::decode(bytes).unwrap();
    assert_eq!(view.shuttle_id(), 100);
    assert_eq!(view.amount(), 200);
    assert_eq!(view.validator(), Some(&[1; 32]));
    assert_eq!(view.encrypted_destination(), &[1, 2, 3, 4]);
    assert_eq!(view.encrypted_destination_capacity(), 72);
    assert_eq!(view.checksum(), 0xBEEF);

    assert_eq!(value.encode().unwrap(), aligned.0.to_vec());

    let mut framed = [0; PrivateTransferFixedArgs::DATA_LEN + 3];
    let remaining_len = value.encode_to(&mut framed).unwrap().len();
    assert_eq!(remaining_len, 3);
    assert_eq!(
        &framed[..PrivateTransferFixedArgs::DATA_LEN],
        aligned.0.as_slice()
    );

    framed[PrivateTransferFixedArgs::DATA_LEN..].copy_from_slice(&[9, 8, 7]);
    let (view, remaining) = PrivateTransferFixedArgs::decode_prefix(&framed).unwrap();
    assert_eq!(view.shuttle_id(), 100);
    assert_eq!(remaining, &[9, 8, 7]);
}

#[test]
fn fixed_offset_layout_rejects_invalid_vec_len() {
    let mut aligned = Aligned([0; PrivateTransferFixedArgs::DATA_LEN]);
    aligned.0[45] = 73;

    assert_eq!(
        PrivateTransferFixedArgs::decode(&aligned.0).unwrap_err(),
        DataLayoutError::LengthExceedsCapacity
    );
}

#[fixed_offset_layout]
struct FixedTrailingVecArgs {
    header: u16,
    #[capacity = 4]
    reserved: Vec<u8>,
    #[flexible = 2]
    tail: Vec<u8>,
}

#[test]
fn fixed_offset_layout_supports_trailing_flexible_vec() {
    assert_eq!(FixedTrailingVecArgs::MIN_DATA_LEN, 7);
    assert_eq!(FixedTrailingVecArgs::MAX_DATA_LEN, 7 + 2 + 0xFFFF);
    assert_eq!(FixedTrailingVecArgs::OFFSETS, [0, 2, 7]);

    let value = FixedTrailingVecArgs {
        header: 7,
        reserved: vec![1, 2],
        tail: vec![9, 8, 7],
    };
    let encoded = value.encode().unwrap();
    assert_eq!(
        encoded,
        [
            7_u16.to_le_bytes().as_slice(),
            &[2, 1, 2, 0, 0],
            3_u16.to_le_bytes().as_slice(),
            &[9, 8, 7],
        ]
        .concat()
    );

    let view = FixedTrailingVecArgs::decode(&encoded).unwrap();
    assert_eq!(view.header(), 7);
    assert_eq!(view.reserved(), &[1, 2]);
    assert_eq!(view.reserved_capacity(), 4);
    assert_eq!(view.tail(), &[9, 8, 7]);

    let empty_tail = FixedTrailingVecArgs {
        header: 7,
        reserved: vec![1, 2],
        tail: vec![],
    };
    let empty_encoded = empty_tail.encode().unwrap();
    assert_eq!(
        empty_encoded,
        [7_u16.to_le_bytes().as_slice(), &[2, 1, 2, 0, 0],].concat()
    );

    let view = FixedTrailingVecArgs::decode(&empty_encoded).unwrap();
    assert_eq!(view.tail(), &[]);
}

#[test]
fn fixed_offset_layout_rejects_invalid_trailing_flexible_vec_encoding() {
    let base = [7_u16.to_le_bytes().as_slice(), &[0, 0, 0, 0, 0]].concat();

    let zero_len_header = [base.as_slice(), 0_u16.to_le_bytes().as_slice()].concat();
    assert_eq!(
        FixedTrailingVecArgs::decode(&zero_len_header).unwrap_err(),
        DataLayoutError::InvalidDataLength
    );

    let truncated_payload = [base.as_slice(), 3_u16.to_le_bytes().as_slice(), &[9]].concat();
    assert_eq!(
        FixedTrailingVecArgs::decode(&truncated_payload).unwrap_err(),
        DataLayoutError::TruncatedVectorPayload
    );
}

#[fixed_offset_layout]
struct FixedTrailingOptionArgs {
    header: u16,
    authority: Address,
    #[flexible]
    delegate: Option<Pubkey>,
}

#[test]
fn fixed_offset_layout_supports_pubkey_and_trailing_flexible_option() {
    assert_eq!(FixedTrailingOptionArgs::MIN_DATA_LEN, 34);
    assert_eq!(FixedTrailingOptionArgs::MAX_DATA_LEN, 67);
    assert_eq!(FixedTrailingOptionArgs::OFFSETS, [0, 2, 34]);

    let none_value = FixedTrailingOptionArgs {
        header: 9,
        authority: Pubkey::from([3; 32]),
        delegate: None,
    };
    let none_encoded = none_value.encode().unwrap();
    assert_eq!(
        none_encoded,
        [9_u16.to_le_bytes().as_slice(), &[3; 32]].concat()
    );
    let view = FixedTrailingOptionArgs::decode(&none_encoded).unwrap();
    assert_eq!(view.delegate(), None);

    let some_value = FixedTrailingOptionArgs {
        header: 9,
        authority: Pubkey::from([3; 32]),
        delegate: Some(Pubkey::from([4; 32])),
    };
    let encoded = some_value.encode().unwrap();
    assert_eq!(
        encoded,
        [9_u16.to_le_bytes().as_slice(), &[3; 32], &[1], &[4; 32],].concat()
    );

    let view = FixedTrailingOptionArgs::decode(&encoded).unwrap();
    assert_eq!(view.header(), 9);
    assert_eq!(view.authority(), &Pubkey::from([3; 32]));
    assert_eq!(view.delegate(), Some(&Pubkey::from([4; 32])));
}

#[test]
fn fixed_offset_layout_rejects_invalid_trailing_flexible_option_encoding() {
    let base = [9_u16.to_le_bytes().as_slice(), &[3; 32]].concat();

    let tagged_none = [base.as_slice(), &[0], &[4; 32]].concat();
    assert_eq!(
        FixedTrailingOptionArgs::decode(&tagged_none).unwrap_err(),
        DataLayoutError::InvalidOptionTag
    );

    let truncated_some = [base.as_slice(), &[1], &[4; 8]].concat();
    assert_eq!(
        FixedTrailingOptionArgs::decode(&truncated_some).unwrap_err(),
        DataLayoutError::TruncatedPayload
    );
}

#[fixed_offset_layout]
struct FixedBoolAndAddressArgs {
    enabled: bool,
    owner: Address,
    sponsored: Option<bool>,
}

#[test]
fn fixed_offset_layout_supports_bool_and_address() {
    assert_eq!(FixedBoolAndAddressArgs::DATA_LEN, 35);

    let value = FixedBoolAndAddressArgs {
        enabled: true,
        owner: Address::from([5; 32]),
        sponsored: Some(false),
    };
    let encoded = value.encode().unwrap();
    assert_eq!(encoded[0], 1);
    assert_eq!(&encoded[1..33], &[5; 32]);
    assert_eq!(&encoded[33..35], &[1, 0]);

    let mut aligned = Aligned([0; FixedBoolAndAddressArgs::DATA_LEN]);
    aligned.0.copy_from_slice(&encoded);

    let view = FixedBoolAndAddressArgs::decode(&aligned.0).unwrap();
    assert!(view.enabled());
    assert_eq!(view.owner(), &Address::from([5; 32]));
    assert_eq!(view.sponsored(), Some(false));
}

#[fixed_offset_layout]
struct FixedAddressVecArgs {
    tag: u8,
    #[capacity = 2]
    owners: Vec<Address>,
    checksum: u16,
}

#[test]
fn fixed_offset_layout_supports_address_vec() {
    assert_eq!(FixedAddressVecArgs::DATA_LEN, 68);
    assert_eq!(FixedAddressVecArgs::OFFSETS, [0, 1, 66]);

    let owners = vec![Address::from([1; 32]), Address::from([2; 32])];
    let value = FixedAddressVecArgs {
        tag: 9,
        owners: owners.clone(),
        checksum: 0xBEEF,
    };
    let encoded = value.encode().unwrap();
    let expected = [
        [9, 2].as_slice(),
        [1; 32].as_slice(),
        [2; 32].as_slice(),
        0xBEEF_u16.to_le_bytes().as_slice(),
    ]
    .concat();
    assert_eq!(encoded.as_slice(), expected.as_slice());

    let mut aligned = Aligned([0; FixedAddressVecArgs::DATA_LEN]);
    aligned.0.copy_from_slice(&encoded);

    let view = FixedAddressVecArgs::decode(&aligned.0).unwrap();
    assert_eq!(view.tag(), 9);
    assert_eq!(view.owners(), owners.as_slice());
    assert_eq!(view.owners_capacity(), 2);
    assert_eq!(view.checksum(), 0xBEEF);
}

#[fixed_offset_layout]
#[derive(Clone)]
struct FixedEntry {
    id: u16,
    enabled: Option<bool>,
}

#[fixed_offset_layout]
struct FixedEntryVecArgs {
    tag: u8,
    #[capacity = 3]
    entries: Vec<FixedEntry>,
    checksum: u16,
}

#[test]
fn fixed_offset_layout_supports_fixed_layout_vec() {
    assert_eq!(FixedEntry::DATA_LEN, 4);
    assert_eq!(FixedEntryVecArgs::DATA_LEN, 16);
    assert_eq!(FixedEntryVecArgs::OFFSETS, [0, 1, 14]);

    let value = FixedEntryVecArgs {
        tag: 9,
        entries: vec![
            FixedEntry {
                id: 0x0102,
                enabled: Some(true),
            },
            FixedEntry {
                id: 0x0304,
                enabled: None,
            },
        ],
        checksum: 0xBEEF,
    };
    let encoded = value.encode().unwrap();
    let expected = [
        [9, 2].as_slice(),
        &[0x02, 0x01, 1, 1],
        &[0x04, 0x03, 0, 0],
        &[0, 0, 0, 0],
        0xBEEF_u16.to_le_bytes().as_slice(),
    ]
    .concat();
    assert_eq!(encoded.as_slice(), expected.as_slice());

    let mut aligned = Aligned([0; FixedEntryVecArgs::DATA_LEN]);
    aligned.0.copy_from_slice(&encoded);

    let view = FixedEntryVecArgs::decode(&aligned.0).unwrap();
    assert_eq!(view.tag(), 9);
    assert_eq!(view.entries_capacity(), 3);
    assert_eq!(view.checksum(), 0xBEEF);

    let entries: wheels::layout::FixedLayoutSlice<'_, FixedEntry> = view.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.get(0).unwrap().id(), 0x0102);
    assert_eq!(entries.get(0).unwrap().enabled(), Some(true));
    assert_eq!(entries.get(1).unwrap().id(), 0x0304);
    assert_eq!(entries.get(1).unwrap().enabled(), None);
    assert_eq!(
        entries.iter().map(|entry| entry.id()).collect::<Vec<_>>(),
        vec![0x0102, 0x0304]
    );
}

#[test]
fn fixed_offset_layout_validates_fixed_layout_vec() {
    let value = FixedEntryVecArgs {
        tag: 9,
        entries: vec![
            FixedEntry {
                id: 0x0102,
                enabled: Some(true),
            },
            FixedEntry {
                id: 0x0304,
                enabled: None,
            },
        ],
        checksum: 0xBEEF,
    };
    let mut encoded = value.encode().unwrap();
    encoded[8] = 2;

    assert_eq!(
        FixedEntryVecArgs::decode(&encoded).unwrap_err(),
        DataLayoutError::InvalidOptionTag
    );

    encoded[1] = 4;
    assert_eq!(
        FixedEntryVecArgs::decode(&encoded).unwrap_err(),
        DataLayoutError::LengthExceedsCapacity
    );
}

#[fixed_offset_layout]
struct FixedTrailingEntryVecArgs {
    tag: u8,
    #[flexible = 1]
    entries: Vec<FixedEntry>,
}

#[test]
fn fixed_offset_layout_supports_trailing_flexible_fixed_layout_vec() {
    assert_eq!(FixedTrailingEntryVecArgs::MIN_DATA_LEN, 1);
    assert_eq!(
        FixedTrailingEntryVecArgs::MAX_DATA_LEN,
        1 + 1 + 0xFF * FixedEntry::DATA_LEN
    );
    assert_eq!(FixedTrailingEntryVecArgs::OFFSETS, [0, 1]);

    let empty = FixedTrailingEntryVecArgs {
        tag: 7,
        entries: vec![],
    };
    let empty_encoded = empty.encode().unwrap();
    assert_eq!(empty_encoded, vec![7]);
    let view = FixedTrailingEntryVecArgs::decode(&empty_encoded).unwrap();
    assert!(view.entries().is_empty());

    let value = FixedTrailingEntryVecArgs {
        tag: 7,
        entries: vec![
            FixedEntry {
                id: 0x0102,
                enabled: Some(false),
            },
            FixedEntry {
                id: 0x0304,
                enabled: Some(true),
            },
        ],
    };
    let encoded = value.encode().unwrap();
    assert_eq!(
        encoded,
        [[7, 2].as_slice(), &[0x02, 0x01, 1, 0], &[0x04, 0x03, 1, 1],].concat()
    );

    let view = FixedTrailingEntryVecArgs::decode(&encoded).unwrap();
    let entries = view.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.get(0).unwrap().enabled(), Some(false));
    assert_eq!(entries.get(1).unwrap().enabled(), Some(true));
}
