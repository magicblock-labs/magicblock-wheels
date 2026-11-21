extern crate alloc;

use std::cell::{Ref, RefCell, RefMut};

use pinocchio::error::ProgramError;
use pinocchio::Address;
use wheels::{
    fixed_offset_layout,
    layout::{
        Decodable, Encodable, LayoutStorage, LayoutStorageMut, MaxLenStorage, PrefixDecodable,
    },
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

struct TestStorage {
    bytes: RefCell<Vec<u8>>,
    resize_lens: RefCell<Vec<usize>>,
}

impl TestStorage {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: RefCell::new(bytes),
            resize_lens: RefCell::new(Vec::new()),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.bytes.borrow().clone()
    }

    fn resize_lens(&self) -> Vec<usize> {
        self.resize_lens.borrow().clone()
    }
}

impl LayoutStorage for TestStorage {
    type Ref<'a>
        = Ref<'a, [u8]>
    where
        Self: 'a;

    fn data_len(&self) -> usize {
        self.bytes.borrow().len()
    }

    fn borrow_data(&self) -> Result<Self::Ref<'_>, ProgramError> {
        Ok(Ref::map(self.bytes.borrow(), Vec::as_slice))
    }
}

impl LayoutStorageMut for TestStorage {
    type RefMut<'a>
        = RefMut<'a, [u8]>
    where
        Self: 'a;

    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError> {
        Ok(RefMut::map(self.bytes.borrow_mut(), Vec::as_mut_slice))
    }

    fn resize(&self, new_len: usize) -> Result<(), ProgramError> {
        self.resize_lens.borrow_mut().push(new_len);
        self.bytes.borrow_mut().resize(new_len, 0);
        Ok(())
    }
}

#[test]
fn max_len_storage_shrinks_exactly() {
    let storage = TestStorage::new(vec![0; 9]);
    let max_len_storage = MaxLenStorage::new(&storage, 16, 4);

    max_len_storage.resize(3).unwrap();

    assert_eq!(storage.resize_lens(), vec![3]);
    assert_eq!(storage.bytes(), vec![0; 3]);

    max_len_storage.resize(16).unwrap();

    assert_eq!(storage.resize_lens(), vec![3, 16]);
    assert_eq!(storage.bytes(), vec![0; 16]);
}

#[derive(Clone)]
#[fixed_offset_layout(buffer_offset = unknown)]
struct AccountConfig {
    enabled: bool,
    authority: Address,
    sponsored: Option<bool>,
    threshold: u16,
}

#[test]
fn account_config_round_trips_and_mutates_scalar_fields() {
    assert_eq!(AccountConfig::DATA_LEN, 37);
    let value = AccountConfig {
        enabled: true,
        authority: Address::from([5; 32]),
        sponsored: Some(false),
        threshold: 9,
    };
    let encoded = value.encode().unwrap();
    assert_eq!(encoded[0], 1);
    assert_eq!(&encoded[1..33], &[5; 32]);
    assert_eq!(&encoded[33..], &[1, 0, 9, 0]);
    let view = AccountConfig::decode(&encoded).unwrap();
    assert!(view.enabled());
    assert_eq!(view.authority(), &value.authority);
    assert_eq!(view.sponsored(), Some(false));

    let storage = TestStorage::new(encoded);
    let mut view = AccountConfig::decode_mut(&storage).unwrap();
    let mut enabled = view.enabled_mut().unwrap();
    assert!(enabled.get().unwrap());
    enabled.set(false).unwrap();
    *view.authority_mut().unwrap() = Address::from([6; 32]);
    let mut sponsored = view.sponsored_mut().unwrap();
    assert_eq!(sponsored.get().unwrap(), Some(false));
    sponsored.set(None).unwrap();
    assert_eq!(sponsored.get().unwrap(), None);
    sponsored.set(Some(true)).unwrap();

    let bytes = storage.bytes();
    let view = AccountConfig::decode(&bytes).unwrap();
    assert!(!view.enabled());
    assert_eq!(view.authority(), &Address::from([6; 32]));
    assert_eq!(view.sponsored(), Some(true));
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedNestedOptionArgs {
    header: u8,
    config: Option<AccountConfig>,
}

#[test]
fn fixed_offset_layout_supports_fixed_nested_option() {
    assert_eq!(FixedNestedOptionArgs::DATA_LEN, 39);

    let none = FixedNestedOptionArgs {
        header: 7,
        config: None,
    }
    .encode()
    .unwrap();
    assert_eq!(none.len(), FixedNestedOptionArgs::DATA_LEN);
    assert_eq!(&none[..2], &[7, 0]);
    assert!(none[2..].iter().all(|byte| *byte == 0));

    let config = AccountConfig {
        sponsored: None,
        authority: Pubkey::from([4; 32]),
        threshold: 9,
        enabled: true,
    };
    let some = FixedNestedOptionArgs {
        header: 7,
        config: Some(config.clone()),
    }
    .encode()
    .unwrap();
    let aligned = aligned_copy::<64>(&some);
    let view = FixedNestedOptionArgs::decode(&aligned.0[..some.len()]).unwrap();
    let config_view = view.config().unwrap();

    assert_eq!(view.header(), 7);
    assert_eq!(config_view.authority(), &config.authority);
    assert_eq!(config_view.threshold(), config.threshold);
    assert_eq!(config_view.enabled(), config.enabled);
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FlexibleNestedOptionArgs {
    header: u8,
    #[flexible]
    config: Option<AccountConfig>,
}

#[test]
fn fixed_offset_layout_supports_mutable_flexible_nested_option() {
    assert_eq!(FlexibleNestedOptionArgs::MIN_DATA_LEN, 1);
    assert_eq!(FlexibleNestedOptionArgs::MAX_DATA_LEN, 39);

    let storage = TestStorage::new(
        FlexibleNestedOptionArgs {
            header: 3,
            config: None,
        }
        .encode()
        .unwrap(),
    );
    let config = AccountConfig {
        sponsored: None,
        authority: Pubkey::from([8; 32]),
        threshold: 11,
        enabled: true,
    };

    {
        let mut view = FlexibleNestedOptionArgs::decode_mut(&storage).unwrap();
        view.config_mut().unwrap().set(Some(&config)).unwrap();
    }

    assert_eq!(
        storage.resize_lens(),
        vec![FlexibleNestedOptionArgs::MAX_DATA_LEN]
    );
    {
        let bytes = storage.bytes();
        let aligned = aligned_copy::<64>(&bytes);
        let view = FlexibleNestedOptionArgs::decode(&aligned.0[..bytes.len()]).unwrap();
        let config_view = view.config().unwrap();
        assert_eq!(config_view.authority(), &config.authority);
        assert_eq!(config_view.threshold(), config.threshold);
        assert!(config_view.enabled());
    }

    {
        let mut view = FlexibleNestedOptionArgs::decode_mut(&storage).unwrap();
        view.config_mut().unwrap().set(None).unwrap();
    }

    assert_eq!(storage.bytes(), vec![3]);
    assert_eq!(
        storage.resize_lens(),
        vec![
            FlexibleNestedOptionArgs::MAX_DATA_LEN,
            FlexibleNestedOptionArgs::MIN_DATA_LEN,
        ]
    );
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingOptionArgs {
    header: u16,
    authority: Address,
    #[flexible]
    delegate: Option<Pubkey>,
}

#[test]
fn trailing_pubkey_option_round_trips_through_storage() {
    assert_eq!(FixedTrailingOptionArgs::MIN_DATA_LEN, 34);
    assert_eq!(FixedTrailingOptionArgs::MAX_DATA_LEN, 67);
    assert_eq!(FixedTrailingOptionArgs::OFFSETS, [0, 2, 34]);

    let delegate = Pubkey::from([4; 32]);
    let mut value = FixedTrailingOptionArgs {
        header: 9,
        authority: Pubkey::from([3; 32]),
        delegate: None,
    };
    let empty = value.encode().unwrap();
    assert_eq!(empty, [9_u16.to_le_bytes().as_slice(), &[3; 32]].concat());
    value.delegate = Some(delegate);
    let populated = value.encode().unwrap();
    assert_eq!(populated, [empty.as_slice(), &[1], &[4; 32]].concat());

    let storage = TestStorage::new(empty.clone());
    let mut view = FixedTrailingOptionArgs::decode_mut(&storage).unwrap();
    let mut slot = view.delegate_mut().unwrap();
    assert_eq!(slot.get().unwrap(), None);
    slot.set(Some(delegate)).unwrap();
    assert_eq!(storage.bytes(), populated);
    let aligned = aligned_copy::<67>(&populated);
    let decoded = FixedTrailingOptionArgs::decode(&aligned.0).unwrap();
    assert_eq!(decoded.header(), value.header);
    assert_eq!(decoded.authority(), &value.authority);
    assert_eq!(decoded.delegate(), Some(&delegate));

    let mut view = FixedTrailingOptionArgs::decode_mut(&storage).unwrap();
    let mut slot = view.delegate_mut().unwrap();
    assert_eq!(slot.get().unwrap(), Some(delegate));
    slot.set(None).unwrap();
    assert_eq!(storage.bytes(), empty);
    let aligned = aligned_copy::<34>(&empty);
    assert_eq!(
        FixedTrailingOptionArgs::decode(&aligned.0)
            .unwrap()
            .delegate(),
        None
    );
    assert_eq!(storage.resize_lens(), [67, 34]);
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

#[fixed_offset_layout(buffer_offset = unknown)]
struct BytePayload {
    version: u8,
    #[extendable = 1]
    payload: Vec<u8>,
}

#[test]
fn byte_payload_extends_by_slices_up_to_the_length_header_limit() {
    assert_eq!(BytePayload::MIN_DATA_LEN, 1);
    assert_eq!(BytePayload::MAX_DATA_LEN, 257);
    let storage = TestStorage::new(vec![7]);
    let mut view = BytePayload::decode_mut(&storage).unwrap();
    let mut payload = view.payload_mut().unwrap();

    payload.extend_from_slice(&[]).unwrap();
    assert_eq!((payload.len(), payload.capacity()), (0, 0));
    let values: Vec<u8> = (0..=254).collect();
    payload.extend_from_slice(&values[..2]).unwrap();
    payload.extend_from_slice(&values[2..4]).unwrap();
    payload.extend_from_slice(&[]).unwrap();
    assert_eq!((payload.len(), payload.capacity()), (4, 4));
    assert_eq!(storage.bytes(), [7, 4, 0, 1, 2, 3]);
    assert_eq!(payload.storage_len(), 5);

    payload.extend_from_slice(&values[4..]).unwrap();
    let full = storage.bytes();
    assert_eq!(
        payload.extend_from_slice(&[255]).unwrap_err(),
        ProgramError::from(DataLayoutError::LengthExceedsCapacity)
    );
    assert_eq!((payload.len(), payload.capacity()), (255, 255));
    assert_eq!(storage.bytes(), full);
    let decoded = BytePayload::decode(&full).unwrap().payload();
    assert_eq!(decoded.as_slice(), values);
    assert_eq!(decoded.storage_len(), 1 + 255);
}

#[test]
fn max_len_storage_grows_extendable_vec_in_steps() {
    let storage = TestStorage::new(vec![7]);
    let max_len_storage = MaxLenStorage::new(&storage, 9, 4);

    {
        let mut view = BytePayload::decode_mut(&max_len_storage).unwrap();
        let mut payload = view.payload_mut().unwrap();

        payload.extend_from_slice(&[1, 2]).unwrap();
        assert_eq!(payload.capacity(), 3);
        assert_eq!(storage.bytes(), [7, 2, 1, 2, 0]);

        payload.push(3).unwrap();
        assert_eq!(storage.resize_lens(), [5]);

        payload.push(4).unwrap();
        assert_eq!(payload.len(), 4);
        assert_eq!(payload.capacity(), 7);
        assert_eq!(payload.storage_len(), 8);
    }

    assert_eq!(storage.resize_lens(), vec![5, 9]);
    let bytes = storage.bytes();
    assert_eq!(bytes, [7, 4, 1, 2, 3, 4, 0, 0, 0]);
    let view = BytePayload::decode(&bytes).unwrap();
    assert_eq!(view.payload().as_slice(), &[1, 2, 3, 4]);
    assert_eq!(view.payload().capacity(), 7);
}

#[test]
fn max_len_storage_rejects_growth_above_max_len() {
    let storage = TestStorage::new(vec![7]);
    let max_len_storage = MaxLenStorage::new(&storage, 4, 4);

    {
        let mut view = BytePayload::decode_mut(&max_len_storage).unwrap();
        let mut payload = view.payload_mut().unwrap();
        assert_eq!(
            payload.extend_from_slice(&[1, 2, 3]).unwrap_err(),
            ProgramError::from(DataLayoutError::LengthExceedsCapacity)
        );
        assert_eq!(payload.len(), 0);
        assert_eq!(payload.capacity(), 0);
    }

    assert_eq!(storage.resize_lens(), Vec::<usize>::new());
    assert_eq!(storage.bytes(), vec![7]);
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FlexibleVariableNestedOptionArgs {
    header: u8,
    #[flexible]
    config: Option<BytePayload>,
}

#[test]
fn fixed_offset_layout_supports_flexible_variable_nested_option() {
    assert_eq!(FlexibleVariableNestedOptionArgs::MIN_DATA_LEN, 1);
    assert_eq!(FlexibleVariableNestedOptionArgs::MAX_DATA_LEN, 259);

    let storage = TestStorage::new(
        FlexibleVariableNestedOptionArgs {
            header: 7,
            config: None,
        }
        .encode()
        .unwrap(),
    );
    let config = BytePayload {
        version: 3,
        payload: vec![9, 8, 7],
    };
    {
        let mut view = FlexibleVariableNestedOptionArgs::decode_mut(&storage).unwrap();
        view.config_mut().unwrap().set(Some(&config)).unwrap();
    }

    assert_eq!(
        FlexibleVariableNestedOptionArgs {
            header: 7,
            config: Some(config),
        }
        .encode()
        .unwrap(),
        vec![7, 1, 3, 3, 9, 8, 7],
    );

    {
        let bytes = storage.bytes();
        let aligned = aligned_copy::<16>(&bytes);
        let view = FlexibleVariableNestedOptionArgs::decode(&aligned.0[..bytes.len()]).unwrap();
        let config_view = view.config().unwrap();

        assert_eq!(config_view.version(), 3);
        assert_eq!(config_view.payload().as_slice(), &[9, 8, 7]);
    }

    let shorter = BytePayload {
        version: 4,
        payload: vec![5],
    };
    {
        let mut view = FlexibleVariableNestedOptionArgs::decode_mut(&storage).unwrap();
        view.config_mut().unwrap().set(Some(&shorter)).unwrap();
    }
    assert_eq!(storage.bytes(), vec![7, 1, 4, 1, 5]);

    {
        let mut view = FlexibleVariableNestedOptionArgs::decode_mut(&storage).unwrap();
        view.config_mut().unwrap().set(None).unwrap();
    }
    assert_eq!(storage.bytes(), vec![7]);
    assert_eq!(storage.resize_lens(), vec![7, 5, 1]);
}

#[test]
fn fixed_offset_layout_rejects_invalid_flexible_nested_option() {
    for (bytes, expected_error) in [
        (vec![7, 0, 3, 1, 9], DataLayoutError::InvalidOptionTag),
        (vec![7, 1, 3, 3, 9], DataLayoutError::LengthExceedsCapacity),
        (vec![7; 260], DataLayoutError::InvalidDataLength),
    ] {
        let aligned = aligned_copy::<264>(&bytes);
        assert_eq!(
            FlexibleVariableNestedOptionArgs::decode(&aligned.0[..bytes.len()]).unwrap_err(),
            expected_error,
        );
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

impl PrivateTransferFixedArgs {
    fn example() -> Self {
        Self {
            shuttle_id: 100,
            amount: 200,
            validator: Some([1; 32]),
            encrypted_destination: vec![1, 2, 3, 4],
            checksum: 0xBEEF,
        }
    }
}

#[test]
fn fixed_capacity_layout_validates_its_wire_format_and_prefix() {
    assert_eq!(PrivateTransferFixedArgs::DATA_LEN, 120);
    assert_eq!(PrivateTransferFixedArgs::OFFSETS, [0, 4, 12, 45, 118]);

    let value = PrivateTransferFixedArgs::example();

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

    aligned.0[PrivateTransferFixedArgs::OFFSETS[3]] = 73;
    assert_eq!(
        PrivateTransferFixedArgs::decode(&aligned.0).unwrap_err(),
        DataLayoutError::LengthExceedsCapacity
    );
}

#[test]
fn fixed_offset_layout_mutates_fixed_fields_and_fixed_capacity_vec() {
    let value = PrivateTransferFixedArgs::example();
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
struct FixedAddressVecArgs {
    tag: u8,
    #[capacity = 2]
    owners: Vec<Address>,
    checksum: u16,
}

#[test]
fn fixed_capacity_addresses_round_trip_and_mutate_in_place() {
    assert_eq!(FixedAddressVecArgs::DATA_LEN, 68);
    assert_eq!(FixedAddressVecArgs::OFFSETS, [0, 1, 66]);

    let value = FixedAddressVecArgs {
        tag: 9,
        owners: vec![Address::from([1; 32]), Address::from([2; 32])],
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

    let aligned = aligned_copy::<{ FixedAddressVecArgs::DATA_LEN }>(&encoded);
    let view = FixedAddressVecArgs::decode(&aligned.0).unwrap();
    assert_eq!(view.tag(), value.tag);
    assert_eq!(view.owners(), value.owners.as_slice());
    assert_eq!(view.owners_capacity(), 2);
    assert_eq!(view.checksum(), value.checksum);

    let storage = TestStorage::new(encoded);
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

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingVecArgs {
    header: u16,
    #[capacity = 4]
    reserved: Vec<u8>,
    #[extendable = 2]
    tail: Vec<u8>,
}

#[test]
fn trailing_byte_vec_decodes_active_elements_and_spare_capacity() {
    assert_eq!(FixedTrailingVecArgs::MIN_DATA_LEN, 7);
    assert_eq!(FixedTrailingVecArgs::MAX_DATA_LEN, 7 + 2 + 0xFFFF);
    assert_eq!(FixedTrailingVecArgs::OFFSETS, [0, 2, 7]);

    let value = FixedTrailingVecArgs {
        header: 7,
        reserved: vec![1, 2],
        tail: vec![9, 8, 7],
    };
    let encoded = value.encode().unwrap();
    assert_eq!(encoded, [7, 0, 2, 1, 2, 0, 0, 3, 0, 9, 8, 7]);
    let aligned = aligned_copy::<16>(&encoded);
    for spare in [0, 4] {
        let storage = &aligned.0[..encoded.len() + spare];
        let view = FixedTrailingVecArgs::decode(storage).unwrap();
        assert_eq!(view.header(), value.header);
        assert_eq!(view.reserved(), value.reserved);
        assert_eq!(view.reserved_capacity(), 4);
        let tail = view.tail();
        assert_eq!(tail.as_slice(), value.tail);
        assert_eq!((tail.len(), tail.capacity()), (3, 3 + spare));
        assert_eq!(tail.encoded_len(), 2 + 3);
        assert_eq!(tail.storage_len(), 2 + 3 + spare);
        assert_eq!(tail.storage_bytes(), &storage[7..]);
    }

    for (truncated_size, expected) in [
        (8, DataLayoutError::MissingLengthHeader),
        (10, DataLayoutError::LengthExceedsCapacity),
    ] {
        assert_eq!(
            FixedTrailingVecArgs::decode(&aligned.0[..truncated_size]).unwrap_err(),
            expected
        );
    }
}

#[test]
fn fixed_offset_layout_mutates_trailing_extendable_vec_storage() {
    let empty = FixedTrailingVecArgs {
        header: 7,
        reserved: vec![1, 2],
        tail: vec![],
    }
    .encode()
    .unwrap();
    assert_eq!(empty, [7, 0, 2, 1, 2, 0, 0]);
    let aligned = aligned_copy::<9>(&empty);
    for size in [empty.len(), empty.len() + 2] {
        let decoded = FixedTrailingVecArgs::decode(&aligned.0[..size]).unwrap();
        let tail = decoded.tail();
        assert!(tail.as_slice().is_empty());
        assert_eq!((tail.len(), tail.capacity()), (0, 0));
    }
    let storage = TestStorage::new(empty);
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

#[fixed_offset_layout(buffer_offset = 1)]
struct FixedTrailingWideVecArgs {
    tag: u8,
    #[extendable = 8]
    payload: Vec<u16>,
}

#[test]
fn wide_length_header_round_trips_an_aligned_u16_slice() {
    const COUNT: usize = 0x1_0000;
    const FRAME_LEN: usize = 1 + 8 + COUNT * 2;

    assert_eq!(FixedTrailingWideVecArgs::MIN_DATA_LEN, 1);
    assert_eq!(
        FixedTrailingWideVecArgs::MAX_DATA_LEN,
        1 + 8 + u32::MAX as usize * 2
    );
    assert_eq!(FixedTrailingWideVecArgs::OFFSETS, [0, 1]);

    let value = FixedTrailingWideVecArgs {
        tag: 7,
        payload: (0..=u16::MAX).collect(),
    };
    let mut aligned = Aligned([0; 1 + FRAME_LEN]);
    // Starting the frame at byte 1 puts the u16 payload at byte 10.
    let frame = &mut aligned.0[1..];
    assert!(value.encode_to(frame).unwrap().is_empty());
    assert_eq!(frame[0], 7);
    assert_eq!(&frame[1..9], &(COUNT as u64).to_le_bytes());
    let view = FixedTrailingWideVecArgs::decode(frame).unwrap();
    assert_eq!(view.tag(), 7);
    assert_eq!(view.payload().len(), COUNT);
    assert_eq!(view.payload().capacity(), COUNT);
    assert_eq!(view.payload().as_slice(), value.payload.as_slice());

    aligned.0.copy_within(1.., 0);
    assert_eq!(
        FixedTrailingWideVecArgs::decode(&aligned.0[..FRAME_LEN]).unwrap_err(),
        DataLayoutError::InvalidBufferOffset
    );
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingU16VecArgs {
    tag: u8,
    #[extendable = 1]
    values: Vec<u16>,
}

#[test]
fn fixed_offset_layout_extends_trailing_extendable_vec_from_non_u8_slice() {
    assert_eq!(FixedTrailingU16VecArgs::MIN_DATA_LEN, 1);
    assert_eq!(FixedTrailingU16VecArgs::MAX_DATA_LEN, 512);

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

#[fixed_offset_layout(buffer_offset = unknown)]
#[derive(Clone)]
struct FixedEntry {
    id: u16,
    enabled: Option<bool>,
}

impl FixedEntry {
    fn new(id: u16, enabled: Option<bool>) -> Self {
        Self { id, enabled }
    }
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedEntryVecArgs {
    tag: u8,
    #[capacity = 3]
    entries: Vec<FixedEntry>,
    checksum: u16,
}

#[test]
fn fixed_entry_vec_encodes_and_validates_its_entries() {
    assert_eq!(FixedEntry::DATA_LEN, 4);
    assert_eq!(FixedEntryVecArgs::DATA_LEN, 16);
    assert_eq!(FixedEntryVecArgs::OFFSETS, [0, 1, 14]);

    let value = FixedEntryVecArgs {
        tag: 9,
        entries: vec![
            FixedEntry::new(0x0102, Some(true)),
            FixedEntry::new(0x0304, None),
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

    let mut aligned = aligned_copy::<{ FixedEntryVecArgs::DATA_LEN }>(&encoded);
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

    for (offset, invalid, expected) in [
        (8, 2, DataLayoutError::InvalidOptionTag),
        (1, 4, DataLayoutError::LengthExceedsCapacity),
    ] {
        aligned.0[offset] = invalid;
        assert_eq!(FixedEntryVecArgs::decode(&aligned.0).unwrap_err(), expected);
        aligned.0[offset] = encoded[offset];
    }
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

        entries.push(&FixedEntry::new(0x0102, Some(false))).unwrap();
        entries.push(&FixedEntry::new(0x0304, Some(true))).unwrap();
        entries.push(&FixedEntry::new(0x0708, None)).unwrap();
        let full = storage.bytes();
        assert_eq!(&full[..6], &[9, 3, 0x02, 0x01, 1, 0]);
        entries.truncate(entries.len()).unwrap();
        assert_eq!(storage.bytes(), full);

        entries.set(0, &FixedEntry::new(0x0506, None)).unwrap();
        entries.pop().unwrap();
        assert_eq!(entries.len(), 2);
        entries.truncate(1).unwrap();
    }

    assert_eq!(storage.data_len(), original_len);
    let bytes = storage.bytes();
    let aligned = aligned_copy::<64>(&bytes);
    let view = FixedEntryVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    let entries = view.entries();
    assert_eq!(entries.len(), 1);
    let first = entries.get(0).unwrap();
    assert_eq!(first.id(), 0x0506);
    assert_eq!(first.enabled(), None);
    assert_eq!(view.checksum(), 0xBEEF);
}

#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingEntryVecArgs {
    tag: u8,
    #[extendable = 1]
    entries: Vec<FixedEntry>,
}

#[test]
fn trailing_entry_vec_round_trips_through_storage() {
    assert_eq!(FixedTrailingEntryVecArgs::MIN_DATA_LEN, 1);
    assert_eq!(
        FixedTrailingEntryVecArgs::MAX_DATA_LEN,
        1 + 1 + 0xFF * FixedEntry::DATA_LEN
    );
    assert_eq!(FixedTrailingEntryVecArgs::OFFSETS, [0, 1]);

    let mut value = FixedTrailingEntryVecArgs {
        tag: 7,
        entries: vec![],
    };
    let empty = value.encode().unwrap();
    assert_eq!(empty, [7]);
    let aligned = aligned_copy::<1>(&empty);
    assert!(FixedTrailingEntryVecArgs::decode(&aligned.0)
        .unwrap()
        .entries()
        .is_empty());

    let storage = TestStorage::new(empty);
    let mut view = FixedTrailingEntryVecArgs::decode_mut(&storage).unwrap();
    let mut entries = view.entries_mut().unwrap();
    assert_eq!((entries.len(), entries.capacity()), (0, 0));
    value.entries = vec![
        FixedEntry::new(0x0102, Some(false)),
        FixedEntry::new(0x0304, Some(true)),
    ];
    for entry in &value.entries {
        entries.push(entry).unwrap();
    }
    let encoded = value.encode().unwrap();
    assert_eq!(encoded, [7, 2, 2, 1, 1, 0, 4, 3, 1, 1]);
    assert_eq!(storage.bytes(), encoded);
    let aligned = aligned_copy::<10>(&encoded);
    let decoded = FixedTrailingEntryVecArgs::decode(&aligned.0).unwrap();
    assert_eq!(decoded.entries().len(), 2);
    for (actual, expected) in decoded.entries().iter().zip(&value.entries) {
        assert_eq!(actual.id(), expected.id);
        assert_eq!(actual.enabled(), expected.enabled);
    }

    entries.set(0, &FixedEntry::new(0x0506, None)).unwrap();
    entries.pop().unwrap();
    assert_eq!((entries.len(), entries.capacity()), (1, 2));
    let bytes = storage.bytes();
    let aligned = aligned_copy::<10>(&bytes);
    let view = FixedTrailingEntryVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    let entries = view.entries();
    assert_eq!((entries.len(), entries.capacity()), (1, 2));
    let first = entries.get(0).unwrap();
    assert_eq!(first.id(), 0x0506);
    assert_eq!(first.enabled(), None);
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
