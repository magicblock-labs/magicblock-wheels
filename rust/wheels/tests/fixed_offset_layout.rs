extern crate alloc;

use std::cell::{Ref, RefCell, RefMut};

use pinocchio::error::ProgramError;
use pinocchio::Address;
use wheels::{
    fixed_offset_layout,
    layout::{Decodable, Encodable, LayoutStorage, LayoutStorageMut, PrefixDecodable},
    DataLayoutError, Pubkey,
};

#[repr(align(8))]
struct Aligned<const N: usize>([u8; N]);

fn aligned_copy<const N: usize>(bytes: &[u8]) -> Aligned<N> {
    assert!(bytes.len() <= N);

    let mut aligned = Aligned([0; N]);
    aligned.0[..bytes.len()].copy_from_slice(bytes);
    aligned
}

struct TestStorage(RefCell<Vec<u8>>);

impl TestStorage {
    fn new(bytes: Vec<u8>) -> Self {
        Self(RefCell::new(bytes))
    }

    fn bytes(&self) -> Vec<u8> {
        self.0.borrow().clone()
    }
}

impl LayoutStorage for TestStorage {
    type Ref<'a>
        = Ref<'a, [u8]>
    where
        Self: 'a;

    fn data_len(&self) -> usize {
        self.0.borrow().len()
    }

    fn borrow_data(&self) -> Result<Self::Ref<'_>, ProgramError> {
        Ok(Ref::map(self.0.borrow(), Vec::as_slice))
    }
}

impl LayoutStorageMut for TestStorage {
    type RefMut<'a>
        = RefMut<'a, [u8]>
    where
        Self: 'a;

    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError> {
        Ok(RefMut::map(self.0.borrow_mut(), Vec::as_mut_slice))
    }

    fn resize(&self, new_len: usize) -> Result<(), ProgramError> {
        self.0.borrow_mut().resize(new_len, 0);
        Ok(())
    }
}

#[fixed_offset_layout(buffer_offset = 0)]
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

    let mut framed = Aligned([0; PrivateTransferFixedArgs::DATA_LEN + 3]);
    let remaining_len = value.encode_to(&mut framed.0).unwrap().len();
    assert_eq!(remaining_len, 3);
    assert_eq!(
        &framed.0[..PrivateTransferFixedArgs::DATA_LEN],
        aligned.0.as_slice()
    );

    framed.0[PrivateTransferFixedArgs::DATA_LEN..].copy_from_slice(&[9, 8, 7]);
    let (view, remaining) = PrivateTransferFixedArgs::decode_prefix(&framed.0).unwrap();
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

#[test]
fn fixed_offset_layout_mutates_fixed_fields_and_fixed_capacity_vec() {
    let value = PrivateTransferFixedArgs {
        shuttle_id: 100,
        amount: 200,
        validator: Some([1; 32]),
        encrypted_destination: vec![1, 2, 3, 4],
        checksum: 0xBEEF,
    };
    let storage = TestStorage::new(value.encode().unwrap());
    let original_len = storage.data_len();
    let mut view = PrivateTransferFixedArgs::decode_mut(&storage).unwrap();
    assert_eq!(view.storage_len(), PrivateTransferFixedArgs::DATA_LEN);

    {
        let mut shuttle_id = view.shuttle_id_mut().unwrap();
        *shuttle_id = 101;
    }
    {
        let mut amount = view.amount_mut().unwrap();
        assert_eq!(amount.get().unwrap(), 200);
        amount.set(202).unwrap();
    }
    {
        let mut validator = view.validator_mut().unwrap();
        assert_eq!(validator.get().unwrap(), Some([1; 32]));
        validator.set(None).unwrap();
        assert_eq!(validator.get().unwrap(), None);
        validator.set(Some([3; 32])).unwrap();
    }
    {
        let mut encrypted_destination = view.encrypted_destination_mut().unwrap();
        assert_eq!(encrypted_destination.len(), 4);
        assert_eq!(encrypted_destination.capacity(), 72);
        assert_eq!(encrypted_destination.get(0).unwrap(), Some(1));

        encrypted_destination.push(5).unwrap();
        assert_eq!(encrypted_destination.pop().unwrap(), Some(5));
        encrypted_destination.set(0, 9).unwrap();
        encrypted_destination.truncate(2).unwrap();
        encrypted_destination.push(8).unwrap();
        assert_eq!(encrypted_destination.len(), 3);
    }
    {
        let mut checksum = view.checksum_mut().unwrap();
        *checksum = 0xCAFE;
    }

    assert_eq!(storage.data_len(), original_len);

    let bytes = storage.bytes();
    let aligned = aligned_copy::<128>(&bytes);
    let view = PrivateTransferFixedArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(view.shuttle_id(), 101);
    assert_eq!(view.amount(), 202);
    assert_eq!(view.validator(), Some(&[3; 32]));
    assert_eq!(view.encrypted_destination(), &[9, 2, 8]);
    assert_eq!(view.encrypted_destination_capacity(), 72);
    assert_eq!(view.checksum(), 0xCAFE);
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingVecArgs {
    header: u16,
    #[capacity = 4]
    reserved: Vec<u8>,
    #[extendable = 2]
    tail: Vec<u8>,
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingByteVecArgs {
    tag: u8,
    #[extendable = 1]
    payload: Vec<u8>,
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingU16VecArgs {
    tag: u8,
    #[extendable = 1]
    values: Vec<u16>,
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingWideVecArgs {
    tag: u8,
    #[extendable = 8]
    payload: Vec<u8>,
}

#[test]
fn fixed_offset_layout_supports_trailing_extendable_vec() {
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

    let aligned = aligned_copy::<64>(&encoded);
    let view = FixedTrailingVecArgs::decode(&aligned.0[..encoded.len()]).unwrap();
    assert_eq!(view.header(), 7);
    assert_eq!(view.reserved(), &[1, 2]);
    assert_eq!(view.reserved_capacity(), 4);
    let tail = view.tail();
    assert_eq!(tail.as_slice(), &[9, 8, 7]);
    assert_eq!(tail.len(), 3);
    assert_eq!(tail.capacity(), 3);

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

    let aligned = aligned_copy::<64>(&empty_encoded);
    let view = FixedTrailingVecArgs::decode(&aligned.0[..empty_encoded.len()]).unwrap();
    let tail = view.tail();
    assert_eq!(tail.as_slice(), &[]);
    assert_eq!(tail.len(), 0);
    assert_eq!(tail.capacity(), 0);
}

#[test]
fn fixed_offset_layout_supports_eight_byte_trailing_extendable_vec_len_width() {
    const PAYLOAD_LEN: usize = 0x1_0000;

    assert_eq!(FixedTrailingWideVecArgs::MIN_DATA_LEN, 1);
    assert_eq!(
        FixedTrailingWideVecArgs::MAX_DATA_LEN,
        1 + 8 + u32::MAX as usize
    );
    assert_eq!(FixedTrailingWideVecArgs::OFFSETS, [0, 1]);

    let payload = (0..PAYLOAD_LEN).map(|value| value as u8).collect();
    let value = FixedTrailingWideVecArgs { tag: 7, payload };
    let encoded = value.encode().unwrap();

    assert_eq!(encoded.len(), 1 + 8 + PAYLOAD_LEN);
    assert_eq!(encoded[0], 7);
    assert_eq!(&encoded[1..9], &(PAYLOAD_LEN as u64).to_le_bytes()[..]);

    let view = FixedTrailingWideVecArgs::decode(&encoded).unwrap();
    assert_eq!(view.tag(), 7);
    assert_eq!(view.payload().len(), PAYLOAD_LEN);
    assert_eq!(view.payload().capacity(), PAYLOAD_LEN);
    assert_eq!(view.payload().active_bytes(), &value.payload[..]);
}

#[test]
fn fixed_offset_layout_rejects_invalid_trailing_extendable_vec_encoding() {
    let base = [7_u16.to_le_bytes().as_slice(), &[0, 0, 0, 0, 0]].concat();

    let empty_storage = [base.as_slice(), 0_u16.to_le_bytes().as_slice()].concat();
    let aligned = aligned_copy::<64>(&empty_storage);
    let view = FixedTrailingVecArgs::decode(&aligned.0[..empty_storage.len()]).unwrap();
    assert_eq!(view.tail().len(), 0);
    assert_eq!(view.tail().capacity(), 0);

    let missing_header = [base.as_slice(), &[9]].concat();
    let aligned = aligned_copy::<64>(&missing_header);
    assert_eq!(
        FixedTrailingVecArgs::decode(&aligned.0[..missing_header.len()]).unwrap_err(),
        DataLayoutError::MissingLengthHeader
    );

    let truncated_payload = [base.as_slice(), 3_u16.to_le_bytes().as_slice(), &[9]].concat();
    let aligned = aligned_copy::<64>(&truncated_payload);
    assert_eq!(
        FixedTrailingVecArgs::decode(&aligned.0[..truncated_payload.len()]).unwrap_err(),
        DataLayoutError::LengthExceedsCapacity
    );
}

#[test]
fn fixed_offset_layout_exposes_trailing_extendable_vec_storage_capacity() {
    let storage = [
        7_u16.to_le_bytes().as_slice(),
        &[2, 1, 2, 0, 0],
        3_u16.to_le_bytes().as_slice(),
        &[9, 8, 7],
        &[0, 0, 0, 0],
    ]
    .concat();
    let aligned = aligned_copy::<64>(&storage);

    let view = FixedTrailingVecArgs::decode(&aligned.0[..storage.len()]).unwrap();
    let tail = view.tail();
    assert_eq!(tail.as_slice(), &[9, 8, 7]);
    assert_eq!(tail.len(), 3);
    assert_eq!(tail.capacity(), 7);
    assert_eq!(tail.encoded_len(), 2 + 3);
    assert_eq!(tail.storage_len(), 2 + 7);
    assert_eq!(
        tail.storage_bytes(),
        [3_u16.to_le_bytes().as_slice(), &[9, 8, 7], &[0, 0, 0, 0]].concat()
    );
}

#[test]
fn fixed_offset_layout_mutates_trailing_extendable_vec_storage() {
    let storage = TestStorage::new([7_u16.to_le_bytes().as_slice(), &[0, 0, 0, 0, 0]].concat());
    let mut view = FixedTrailingVecArgs::decode_mut(&storage).unwrap();

    let mut tail = view.tail_mut().unwrap();
    assert_eq!(tail.len(), 0);
    assert_eq!(tail.capacity(), 0);

    tail.push(9).unwrap();
    tail.push(8).unwrap();
    tail.set(1, 7).unwrap();
    assert_eq!(tail.pop().unwrap(), Some(7));
    tail.push(6).unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail.capacity(), 2);

    let bytes = storage.bytes();
    let aligned = aligned_copy::<64>(&bytes);
    let view = FixedTrailingVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(view.tail().as_slice(), &[9, 6]);

    let mut view = FixedTrailingVecArgs::decode_mut(&storage).unwrap();
    let mut tail = view.tail_mut().unwrap();
    tail.clear().unwrap();
    assert_eq!(tail.len(), 0);
    assert_eq!(tail.capacity(), 2);

    let bytes = storage.bytes();
    let aligned = aligned_copy::<64>(&bytes);
    let view = FixedTrailingVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(view.tail().len(), 0);
    assert_eq!(view.tail().capacity(), 2);
}

#[test]
fn fixed_offset_layout_extends_trailing_extendable_vec_from_slice() {
    let storage = TestStorage::new(vec![7]);
    let mut view = FixedTrailingByteVecArgs::decode_mut(&storage).unwrap();

    {
        let mut payload = view.payload_mut().unwrap();
        assert_eq!(payload.len(), 0);
        assert_eq!(payload.capacity(), 0);

        payload.extend_from_slice(&[]).unwrap();
        assert_eq!(payload.len(), 0);
        assert_eq!(payload.capacity(), 0);

        payload.extend_from_slice(&[9, 8]).unwrap();
        payload.extend_from_slice(&[7, 6]).unwrap();
        payload.extend_from_slice(&[]).unwrap();
        assert_eq!(payload.len(), 4);
        assert_eq!(payload.capacity(), 4);
    }

    let bytes = storage.bytes();
    assert_eq!(bytes, vec![7, 4, 9, 8, 7, 6]);

    let aligned = aligned_copy::<16>(&bytes);
    let view = FixedTrailingByteVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(view.payload().as_slice(), &[9, 8, 7, 6]);
    assert_eq!(view.payload().storage_len(), 1 + 4);
}

#[test]
fn fixed_offset_layout_extends_trailing_extendable_vec_from_non_u8_slice() {
    let storage = TestStorage::new(vec![5]);
    let mut view = FixedTrailingU16VecArgs::decode_mut(&storage).unwrap();

    {
        let mut values = view.values_mut().unwrap();
        values.extend_from_slice(&[0x0102, 0x0304]).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values.capacity(), 2);
    }

    let bytes = storage.bytes();
    assert_eq!(bytes, vec![5, 2, 0x02, 0x01, 0x04, 0x03]);

    let aligned = aligned_copy::<16>(&bytes);
    let view = FixedTrailingU16VecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(view.values().as_slice(), &[0x0102, 0x0304]);
}

#[test]
fn fixed_offset_layout_extend_from_slice_rejects_len_width_overflow() {
    let storage = TestStorage::new(vec![9]);
    let values = (0..=254).map(|value| value as u8).collect::<Vec<_>>();

    {
        let mut view = FixedTrailingByteVecArgs::decode_mut(&storage).unwrap();
        let mut payload = view.payload_mut().unwrap();
        payload.extend_from_slice(&values).unwrap();
        assert_eq!(payload.len(), 255);
        assert_eq!(payload.capacity(), 255);
    }

    let bytes_before_overflow = storage.bytes();
    {
        let mut view = FixedTrailingByteVecArgs::decode_mut(&storage).unwrap();
        let mut payload = view.payload_mut().unwrap();
        assert_eq!(
            payload.extend_from_slice(&[255]).unwrap_err(),
            ProgramError::from(DataLayoutError::LengthExceedsCapacity)
        );
        assert_eq!(payload.len(), 255);
        assert_eq!(payload.capacity(), 255);
    }

    assert_eq!(storage.bytes(), bytes_before_overflow);
    let aligned = aligned_copy::<512>(&bytes_before_overflow);
    let view = FixedTrailingByteVecArgs::decode(&aligned.0[..bytes_before_overflow.len()]).unwrap();
    assert_eq!(view.payload().as_slice(), values.as_slice());
}

#[fixed_offset_layout(buffer_offset = 0)]
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
    let aligned = aligned_copy::<96>(&none_encoded);
    let view = FixedTrailingOptionArgs::decode(&aligned.0[..none_encoded.len()]).unwrap();
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

    let aligned = aligned_copy::<96>(&encoded);
    let view = FixedTrailingOptionArgs::decode(&aligned.0[..encoded.len()]).unwrap();
    assert_eq!(view.header(), 9);
    assert_eq!(view.authority(), &Pubkey::from([3; 32]));
    assert_eq!(view.delegate(), Some(&Pubkey::from([4; 32])));
}

#[test]
fn fixed_offset_layout_rejects_invalid_trailing_flexible_option_encoding() {
    let base = [9_u16.to_le_bytes().as_slice(), &[3; 32]].concat();

    let tagged_none = [base.as_slice(), &[0], &[4; 32]].concat();
    let aligned = aligned_copy::<96>(&tagged_none);
    assert_eq!(
        FixedTrailingOptionArgs::decode(&aligned.0[..tagged_none.len()]).unwrap_err(),
        DataLayoutError::InvalidOptionTag
    );

    let truncated_some = [base.as_slice(), &[1], &[4; 8]].concat();
    let aligned = aligned_copy::<96>(&truncated_some);
    assert_eq!(
        FixedTrailingOptionArgs::decode(&aligned.0[..truncated_some.len()]).unwrap_err(),
        DataLayoutError::TruncatedPayload
    );
}

#[test]
fn fixed_offset_layout_mutates_trailing_flexible_option_storage() {
    let none_value = FixedTrailingOptionArgs {
        header: 9,
        authority: Pubkey::from([3; 32]),
        delegate: None,
    };
    let storage = TestStorage::new(none_value.encode().unwrap());
    assert_eq!(storage.data_len(), FixedTrailingOptionArgs::MIN_DATA_LEN);

    {
        let mut view = FixedTrailingOptionArgs::decode_mut(&storage).unwrap();
        let mut delegate = view.delegate_mut().unwrap();
        assert_eq!(delegate.get().unwrap(), None);
        delegate.set(Some(Pubkey::from([4; 32]))).unwrap();
    }
    assert_eq!(storage.data_len(), FixedTrailingOptionArgs::MAX_DATA_LEN);

    let bytes = storage.bytes();
    let aligned = aligned_copy::<96>(&bytes);
    let view = FixedTrailingOptionArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(view.delegate(), Some(&Pubkey::from([4; 32])));

    {
        let mut view = FixedTrailingOptionArgs::decode_mut(&storage).unwrap();
        let mut delegate = view.delegate_mut().unwrap();
        delegate.set(None).unwrap();
    }
    assert_eq!(storage.data_len(), FixedTrailingOptionArgs::MIN_DATA_LEN);

    let bytes = storage.bytes();
    let aligned = aligned_copy::<96>(&bytes);
    let view = FixedTrailingOptionArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(view.delegate(), None);
}

#[fixed_offset_layout(buffer_offset = 0)]
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

#[test]
fn fixed_offset_layout_mutates_bool_address_and_option_bool_slots() {
    let value = FixedBoolAndAddressArgs {
        enabled: true,
        owner: Address::from([5; 32]),
        sponsored: Some(false),
    };
    let storage = TestStorage::new(value.encode().unwrap());
    let mut view = FixedBoolAndAddressArgs::decode_mut(&storage).unwrap();

    {
        let mut enabled = view.enabled_mut().unwrap();
        assert!(enabled.get().unwrap());
        enabled.set(false).unwrap();
    }
    {
        let mut owner = view.owner_mut().unwrap();
        *owner = Address::from([6; 32]);
    }
    {
        let mut sponsored = view.sponsored_mut().unwrap();
        assert_eq!(sponsored.get().unwrap(), Some(false));
        sponsored.set(None).unwrap();
        assert_eq!(sponsored.get().unwrap(), None);
        sponsored.set(Some(true)).unwrap();
    }

    let bytes = storage.bytes();
    let aligned = aligned_copy::<64>(&bytes);
    let view = FixedBoolAndAddressArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert!(!view.enabled());
    assert_eq!(view.owner(), &Address::from([6; 32]));
    assert_eq!(view.sponsored(), Some(true));
}

#[fixed_offset_layout(buffer_offset = 1)]
struct FixedOffsetOneCopyArgs {
    amount: u64,
    counter: u32,
}

#[test]
fn fixed_offset_layout_validates_buffer_offset() {
    let mut aligned = Aligned([0; FixedOffsetOneCopyArgs::DATA_LEN + 1]);
    let bytes = &mut aligned.0;
    bytes[1..9].copy_from_slice(&55_u64.to_le_bytes());
    bytes[9..13].copy_from_slice(&7_u32.to_le_bytes());

    let view = FixedOffsetOneCopyArgs::decode(&bytes[1..]).unwrap();
    assert_eq!(view.amount(), 55);
    assert_eq!(view.counter(), 7);
    assert_eq!(
        FixedOffsetOneCopyArgs::decode(&bytes[..FixedOffsetOneCopyArgs::DATA_LEN]).unwrap_err(),
        DataLayoutError::InvalidBufferOffset
    );
}

#[fixed_offset_layout(buffer_offset = 0)]
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

#[test]
fn fixed_offset_layout_mutates_fixed_capacity_value_vec_storage() {
    let value = FixedAddressVecArgs {
        tag: 9,
        owners: vec![Address::from([1; 32]), Address::from([2; 32])],
        checksum: 0xBEEF,
    };
    let storage = TestStorage::new(value.encode().unwrap());
    let original_len = storage.data_len();
    let mut view = FixedAddressVecArgs::decode_mut(&storage).unwrap();

    {
        let mut owners = view.owners_mut().unwrap();
        assert_eq!(owners.len(), 2);
        assert_eq!(owners.capacity(), 2);
        assert_eq!(owners.get(0).unwrap(), Some(Address::from([1; 32])));
        assert_eq!(
            owners.push(Address::from([3; 32])).unwrap_err(),
            ProgramError::from(DataLayoutError::LengthExceedsCapacity)
        );

        assert_eq!(owners.pop().unwrap(), Some(Address::from([2; 32])));
        owners.set(0, Address::from([4; 32])).unwrap();
        owners.push(Address::from([5; 32])).unwrap();
    }

    assert_eq!(storage.data_len(), original_len);
    let bytes = storage.bytes();
    let aligned = aligned_copy::<96>(&bytes);
    let view = FixedAddressVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    assert_eq!(
        view.owners(),
        &[Address::from([4; 32]), Address::from([5; 32])]
    );
    assert_eq!(view.checksum(), 0xBEEF);
}

#[fixed_offset_layout(buffer_offset = unknown)]
#[derive(Clone)]
struct FixedEntry {
    id: u16,
    enabled: Option<bool>,
}

#[fixed_offset_layout(buffer_offset = 0)]
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

    let aligned = aligned_copy::<64>(&encoded);
    assert_eq!(
        FixedEntryVecArgs::decode(&aligned.0[..encoded.len()]).unwrap_err(),
        DataLayoutError::InvalidOptionTag
    );

    encoded[1] = 4;
    let aligned = aligned_copy::<64>(&encoded);
    assert_eq!(
        FixedEntryVecArgs::decode(&aligned.0[..encoded.len()]).unwrap_err(),
        DataLayoutError::LengthExceedsCapacity
    );
}

#[test]
fn fixed_offset_layout_mutates_fixed_capacity_fixed_layout_vec_storage() {
    let value = FixedEntryVecArgs {
        tag: 9,
        entries: vec![],
        checksum: 0xBEEF,
    };
    let storage = TestStorage::new(value.encode().unwrap());
    let original_len = storage.data_len();
    let mut view = FixedEntryVecArgs::decode_mut(&storage).unwrap();

    {
        let mut entries = view.entries_mut().unwrap();
        assert_eq!(entries.len(), 0);
        assert_eq!(entries.capacity(), 3);

        entries
            .push(&FixedEntry {
                id: 0x0102,
                enabled: Some(false),
            })
            .unwrap();
        entries
            .push(&FixedEntry {
                id: 0x0304,
                enabled: Some(true),
            })
            .unwrap();
        entries
            .set(
                1,
                &FixedEntry {
                    id: 0x0506,
                    enabled: None,
                },
            )
            .unwrap();
        entries.pop().unwrap();
        entries.truncate(1).unwrap();
    }

    assert_eq!(storage.data_len(), original_len);
    let bytes = storage.bytes();
    let aligned = aligned_copy::<64>(&bytes);
    let view = FixedEntryVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    let entries = view.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries.get(0).unwrap().id(), 0x0102);
    assert_eq!(entries.get(0).unwrap().enabled(), Some(false));
    assert_eq!(view.checksum(), 0xBEEF);
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingEntryVecArgs {
    tag: u8,
    #[extendable = 1]
    entries: Vec<FixedEntry>,
}

#[test]
fn fixed_offset_layout_supports_trailing_extendable_fixed_layout_vec() {
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
    let aligned = aligned_copy::<64>(&empty_encoded);
    let view = FixedTrailingEntryVecArgs::decode(&aligned.0[..empty_encoded.len()]).unwrap();
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

    let aligned = aligned_copy::<64>(&encoded);
    let view = FixedTrailingEntryVecArgs::decode(&aligned.0[..encoded.len()]).unwrap();
    let entries = view.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.get(0).unwrap().enabled(), Some(false));
    assert_eq!(entries.get(1).unwrap().enabled(), Some(true));
}

#[test]
fn fixed_offset_layout_mutates_trailing_extendable_fixed_layout_vec_storage() {
    let storage = TestStorage::new(vec![7]);
    let mut view = FixedTrailingEntryVecArgs::decode_mut(&storage).unwrap();

    let mut entries = view.entries_mut().unwrap();
    assert_eq!(entries.len(), 0);
    assert_eq!(entries.capacity(), 0);

    entries
        .push(&FixedEntry {
            id: 0x0102,
            enabled: Some(false),
        })
        .unwrap();
    entries
        .push(&FixedEntry {
            id: 0x0304,
            enabled: Some(true),
        })
        .unwrap();
    entries
        .set(
            1,
            &FixedEntry {
                id: 0x0506,
                enabled: None,
            },
        )
        .unwrap();
    entries.pop().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries.capacity(), 2);

    let bytes = storage.bytes();
    let aligned = aligned_copy::<64>(&bytes);
    let view = FixedTrailingEntryVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    let entries = view.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries.capacity(), 2);
    assert_eq!(entries.get(0).unwrap().id(), 0x0102);
    assert_eq!(entries.get(0).unwrap().enabled(), Some(false));
}

#[test]
fn fixed_offset_layout_rejects_non_element_sized_trailing_storage() {
    let storage = [7, 0, 0].as_slice();
    let aligned = aligned_copy::<64>(storage);

    assert_eq!(
        FixedTrailingEntryVecArgs::decode(&aligned.0[..storage.len()]).unwrap_err(),
        DataLayoutError::InvalidDataLength
    );
}
