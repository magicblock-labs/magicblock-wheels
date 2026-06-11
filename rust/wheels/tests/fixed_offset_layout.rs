extern crate alloc;

use pinocchio::{error::ProgramError, Address};
use wheels::{fixed_offset_layout, Pubkey};

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

    assert_eq!(value.encode(), Ok(aligned.0));
}

#[test]
fn fixed_offset_layout_rejects_invalid_vec_len() {
    let mut aligned = Aligned([0; PrivateTransferFixedArgs::DATA_LEN]);
    aligned.0[45] = 73;

    assert_eq!(
        PrivateTransferFixedArgs::decode(&aligned.0).unwrap_err(),
        ProgramError::InvalidInstructionData
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
    assert_eq!(
        none_value.encode().unwrap(),
        [9_u16.to_le_bytes().as_slice(), &[3; 32]].concat()
    );

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
