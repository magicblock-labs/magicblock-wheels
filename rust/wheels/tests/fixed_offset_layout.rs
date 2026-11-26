extern crate alloc;

use std::{
    cell::{Ref, RefCell, RefMut},
    ops::{Deref, DerefMut},
};

use pinocchio::error::ProgramError;
use pinocchio::Address;
use wheels::{
    fixed_offset_layout,
    layout::{
        Decodable, Encodable, LayoutStorage, LayoutStorageMut, LayoutWriteGuard, MaxLenStorage,
        PrefixDecodable,
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
        let bytes = self
            .bytes
            .try_borrow()
            .map_err(|_| ProgramError::AccountBorrowFailed)?;
        Ok(Ref::map(bytes, Vec::as_slice))
    }
}

impl LayoutStorageMut for TestStorage {
    type RefMut<'a>
        = TestWriteGuard<'a>
    where
        Self: 'a;

    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError> {
        Ok(TestWriteGuard {
            borrowed: self
                .bytes
                .try_borrow_mut()
                .map_err(|_| ProgramError::AccountBorrowFailed)?,
            resize_lens: &self.resize_lens,
        })
    }
}

struct TestWriteGuard<'a> {
    borrowed: RefMut<'a, Vec<u8>>,
    resize_lens: &'a RefCell<Vec<usize>>,
}

impl Deref for TestWriteGuard<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.borrowed
    }
}

impl DerefMut for TestWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.borrowed
    }
}

impl LayoutWriteGuard for TestWriteGuard<'_> {
    fn resize(&mut self, new_len: usize) -> Result<(), ProgramError> {
        self.resize_lens.borrow_mut().push(new_len);
        self.borrowed.resize(new_len, 0);
        Ok(())
    }
}

#[test]
fn max_len_storage_shrinks_exactly_and_grows_in_steps() {
    let storage = TestStorage::new(vec![0; 9]);
    let max_len_storage = MaxLenStorage::new(&storage, 16, 4);

    max_len_storage.resize(3).unwrap();

    assert_eq!(storage.resize_lens(), vec![3]);
    assert_eq!(storage.bytes(), vec![0; 3]);

    max_len_storage.resize(4).unwrap();
    assert_eq!(storage.data_len(), 7);

    max_len_storage.resize(16).unwrap();
    assert_eq!(
        max_len_storage.resize(17).unwrap_err(),
        ProgramError::from(DataLayoutError::LengthExceedsCapacity)
    );
    assert_eq!(storage.resize_lens(), vec![3, 7, 16]);
    assert_eq!(storage.bytes(), vec![0; 16]);
}

#[derive(Clone)]
#[fixed_offset_layout(buffer_offset = any)]
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
    let reader = storage.borrow_data().unwrap();
    assert_eq!(
        AccountConfig::decode_mut(&storage).unwrap_err(),
        ProgramError::AccountBorrowFailed
    );
    drop(reader);
    let mut view = AccountConfig::decode_mut(&storage).unwrap();
    assert_eq!(
        storage.borrow_data().unwrap_err(),
        ProgramError::AccountBorrowFailed
    );
    let mut enabled = view.enabled_mut();
    assert!(enabled.get());
    enabled.set(false);
    *view.authority_mut() = Address::from([6; 32]);
    let mut sponsored = view.sponsored_mut();
    assert_eq!(sponsored.get(), Some(false));
    sponsored.set(None);
    assert_eq!(sponsored.get(), None);
    sponsored.set(Some(true));
    drop(view);

    let bytes = storage.bytes();
    let view = AccountConfig::decode(&bytes).unwrap();
    assert!(!view.enabled());
    assert_eq!(view.authority(), &Address::from([6; 32]));
    assert_eq!(view.sponsored(), Some(true));
}

#[fixed_offset_layout(buffer_offset = any)]
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

#[fixed_offset_layout(buffer_offset = any)]
struct FixedNestedOptionVecArgs {
    _padding: [u8; 7],
    #[capacity = 2]
    entries: Vec<FixedNestedOptionArgs>,
}

#[test]
fn packed_nested_entries_round_trip() {
    assert_eq!(FixedNestedOptionVecArgs::DATA_LEN, 86);
    let storage = TestStorage::new(vec![0; FixedNestedOptionVecArgs::DATA_LEN]);
    let mut view = FixedNestedOptionVecArgs::decode_mut(&storage).unwrap();
    let mut entries = view.entries_mut();
    let first = FixedNestedOptionArgs {
        header: 11,
        config: None,
    };
    let second = FixedNestedOptionArgs {
        header: 22,
        config: None,
    };

    // The first entry starts at byte 8. Its 39-byte width puts the second at byte 47.
    entries.push(&first).unwrap();
    entries.push(&second).unwrap();

    let decoded = FixedNestedOptionVecArgs::decode(view.bytes()).unwrap();
    let entries = decoded.entries();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.get(0).unwrap().header(), first.header);
    assert_eq!(entries.get(1).unwrap().header(), second.header);
}

#[test]
fn single_packed_slot_only_needs_its_first_offset_to_match() {
    #[fixed_offset_layout(buffer_offset = 0)]
    struct Child {
        key: [u8; 32],
        tag: [u8; 7],
    }
    #[fixed_offset_layout(buffer_offset = 0)]
    struct Parent {
        pad: [u8; 7],
        #[capacity = 1]
        entries: Vec<Child>,
    }

    let mut bytes = Aligned([0; Parent::DATA_LEN]);
    let record = Parent {
        pad: [0; 7],
        entries: vec![Child {
            key: [42; 32],
            tag: [0; 7],
        }],
    };
    assert!(record.encode_to(&mut bytes.0).unwrap().is_empty());
    assert_eq!(
        Parent::decode(&bytes.0)
            .unwrap()
            .entries()
            .get(0)
            .unwrap()
            .key(),
        &[42; 32]
    );
}

#[test]
fn packed_extendable_slots_account_for_both_headers() {
    #[fixed_offset_layout(buffer_offset = 2)]
    struct Child {
        id: u64,
    }
    #[fixed_offset_layout(buffer_offset = 0)]
    struct Parent {
        #[extendable = 1]
        entries: Vec<Child>,
    }

    let record = Parent {
        entries: vec![Child { id: 1 }, Child { id: 2 }],
    };
    let mut bytes = Aligned([0; Parent::MIN_DATA_LEN + 2 * Child::DATA_LEN]);
    assert!(record.encode_to(&mut bytes.0).unwrap().is_empty());
    let parent = Parent::decode(&bytes.0).unwrap();
    assert_eq!(parent.entries().get(1).unwrap().id(), 2);
}

#[fixed_offset_layout(buffer_offset = any)]
struct FlexibleNestedOptionArgs {
    header: u8,
    #[flexible]
    config: Option<AccountConfig>,
}

#[test]
fn flexible_option_round_trips_a_fixed_size_child() {
    assert_eq!(FlexibleNestedOptionArgs::MIN_DATA_LEN, 2);
    assert_eq!(FlexibleNestedOptionArgs::MAX_DATA_LEN, 39);

    let empty = FlexibleNestedOptionArgs {
        header: 7,
        config: None,
    }
    .encode()
    .unwrap();
    assert_eq!(empty, [7, 0]);
    let storage = TestStorage::new(empty);
    let config = AccountConfig {
        sponsored: None,
        authority: Pubkey::from([8; 32]),
        threshold: 11,
        enabled: true,
    };
    let mut view = FlexibleNestedOptionArgs::decode_mut(&storage).unwrap();
    view.config_mut().set(Some(&config)).unwrap();
    drop(view);

    let bytes = storage.bytes();
    assert_eq!(bytes.len(), FlexibleNestedOptionArgs::MAX_DATA_LEN);
    assert_eq!(&bytes[..2], &[7, 1]);
    let aligned = aligned_copy::<{ FlexibleNestedOptionArgs::MAX_DATA_LEN }>(&bytes);
    let view = FlexibleNestedOptionArgs::decode(&aligned.0).unwrap();
    assert_eq!(view.config().unwrap().threshold(), config.threshold);
}

#[fixed_offset_layout(buffer_offset = any)]
struct FlexiblePubkeyArgs {
    header: u8,
    #[flexible]
    key: Option<Pubkey>,
}

#[test]
fn flexible_pubkey_round_trips_after_set_and_clear() {
    assert_eq!(FlexiblePubkeyArgs::MIN_DATA_LEN, 2);
    assert_eq!(FlexiblePubkeyArgs::MAX_DATA_LEN, 34);

    let storage = TestStorage::new(vec![7, 0]);
    let capped = MaxLenStorage::new(&storage, 64, 64);
    let mut view = FlexiblePubkeyArgs::decode_mut(&capped).unwrap();
    for key in [Some(Pubkey::from([4; 32])), None] {
        let mut slot = view.key_mut();
        slot.set(key).unwrap();
        assert_eq!(slot.get(), key);

        let decoded = FlexiblePubkeyArgs::decode(view.bytes()).unwrap();
        assert_eq!(decoded.header(), 7);
        assert_eq!(decoded.key(), key.as_ref());

        let record = FlexiblePubkeyArgs { header: 7, key };
        let next = BytePayload {
            version: 9,
            payload: vec![42],
        };
        let mut bytes = vec![0xff; record.encoded_len().unwrap() + next.encoded_len().unwrap()];
        let rest = record.encode_to(&mut bytes).unwrap();
        assert!(next.encode_to(rest).unwrap().is_empty());
        let (first, rest) = FlexiblePubkeyArgs::decode_prefix(&bytes).unwrap();
        assert_eq!(first.bytes(), view.bytes());
        assert_eq!(first.key(), key.as_ref());
        assert_eq!(rest, &[9, 1, 1, 42]);
        assert_eq!(
            BytePayload::decode(rest).unwrap().payload().as_slice(),
            &[42]
        );
    }
}

#[test]
fn flexible_pubkey_rejects_invalid_tag_and_truncated_key() {
    assert_eq!(
        FlexiblePubkeyArgs::decode(&[7]).unwrap_err(),
        DataLayoutError::InvalidDataLength
    );
    let stale = [0; 34];
    let (empty, rest) = FlexiblePubkeyArgs::decode_prefix(&stale).unwrap();
    assert_eq!(empty.key(), None);
    assert_eq!(rest.len(), 32);
    let mut bytes = [7; FlexiblePubkeyArgs::MAX_DATA_LEN];
    for (tag, key_bytes, expected) in [
        (2, 32, DataLayoutError::InvalidOptionTag),
        (1, 8, DataLayoutError::TruncatedPayload),
    ] {
        bytes[1] = tag;
        assert_eq!(
            FlexiblePubkeyArgs::decode(&bytes[..2 + key_bytes]).unwrap_err(),
            expected
        );
        let storage = TestStorage::new(bytes[..2 + key_bytes].to_vec());
        assert_eq!(
            FlexiblePubkeyArgs::decode_mut(&storage).unwrap_err(),
            ProgramError::from(expected)
        );
        assert!(storage.borrow_data_mut().is_ok());
    }
}

#[fixed_offset_layout(buffer_offset = any)]
struct BytePayload {
    version: u8,
    #[extendable = 1]
    payload: Vec<u8>,
}

#[test]
fn byte_payload_extends_by_slices_up_to_the_length_header_limit() {
    assert_eq!(BytePayload::MIN_DATA_LEN, 3);
    assert_eq!(BytePayload::MAX_DATA_LEN, 258);
    // Successful rows give (len, capacity).
    for (bytes, expected) in [
        (&[7][..], Err(DataLayoutError::MissingLengthHeader)),
        (&[7, 0], Err(DataLayoutError::MissingCapacityHeader)),
        (&[7, 0, 0], Ok((0, 0))),
        (&[7, 2, 1, 42], Err(DataLayoutError::LengthExceedsCapacity)),
        (&[7, 0, 2, 42], Err(DataLayoutError::TruncatedVectorPayload)),
        (&[7, 0, 2, 0, 0], Ok((0, 2))),
        (&[7, 1, 2, 42, 0], Ok((1, 2))),
    ] {
        let decoded = BytePayload::decode_prefix(bytes).map(|(view, rest)| {
            assert!(rest.is_empty());
            let payload = view.payload();
            (payload.len(), payload.capacity())
        });
        assert_eq!(decoded, expected, "{bytes:?}");

        let storage = TestStorage::new(bytes.to_vec());
        let decoded = BytePayload::decode_prefix_mut(&storage)
            .map(|mut view| {
                let payload = view.payload_mut();
                (payload.len(), payload.capacity())
            })
            .map_err(|error| DataLayoutError::try_from(error).expect("expected a layout error"));
        assert_eq!(decoded, expected, "{bytes:?}");
    }
    let storage = TestStorage::new(vec![7, 0, 0]);
    let mut view = BytePayload::decode_mut(&storage).unwrap();
    let mut payload = view.payload_mut();

    payload.extend_from_slice(&[]).unwrap();
    assert_eq!((payload.len(), payload.capacity()), (0, 0));
    let values: Vec<u8> = (0..=254).collect();
    payload.extend_from_slice(&values[..2]).unwrap();
    payload.extend_from_slice(&values[2..4]).unwrap();
    assert_eq!((payload.len(), payload.capacity()), (4, 4));
    assert_eq!(payload.storage_len(), 6);
    assert_eq!(view.bytes(), [7, 4, 4, 0, 1, 2, 3]);
    let mut payload = view.payload_mut();

    payload.extend_from_slice(&values[4..]).unwrap();
    let full = view.bytes().to_vec();
    let mut payload = view.payload_mut();
    assert_eq!(
        payload.extend_from_slice(&[255]).unwrap_err(),
        ProgramError::from(DataLayoutError::LengthExceedsCapacity)
    );
    assert_eq!((payload.len(), payload.capacity()), (255, 255));
    drop(view);
    assert_eq!(storage.bytes(), full);
    let decoded = BytePayload::decode(&full).unwrap().payload();
    assert_eq!(decoded.as_slice(), values);
    assert_eq!(decoded.storage_len(), 2 + 255);
}

#[test]
fn max_len_storage_grows_extendable_vec_in_steps_up_to_its_limit() {
    let storage = TestStorage::new(vec![7, 0, 0]);
    let max_len_storage = MaxLenStorage::new(&storage, 10, 4);
    let mut view = BytePayload::decode_mut(&max_len_storage).unwrap();
    let mut payload = view.payload_mut();

    assert_eq!(
        payload.extend_from_slice(&[0; 8]).unwrap_err(),
        ProgramError::from(DataLayoutError::LengthExceedsCapacity)
    );
    assert_eq!((payload.len(), payload.capacity()), (0, 0));
    assert!(storage.resize_lens().is_empty());
    assert_eq!(view.bytes(), [7, 0, 0]);

    let mut payload = view.payload_mut();
    payload.extend_from_slice(&[1, 2]).unwrap();
    assert_eq!(payload.capacity(), 4);
    assert_eq!(view.bytes(), [7, 2, 4, 1, 2, 0, 0]);
    let mut payload = view.payload_mut();
    payload.push(3).unwrap();
    assert_eq!(storage.resize_lens(), [7]);
    payload.push(4).unwrap();
    payload.push(5).unwrap();
    assert_eq!(payload.storage_len(), 9);
    assert_eq!(storage.resize_lens(), [7, 10]);
    drop(view);

    let bytes = storage.bytes();
    assert_eq!(bytes, [7, 5, 7, 1, 2, 3, 4, 5, 0, 0]);
    let view = BytePayload::decode(&bytes).unwrap();
    assert_eq!(view.payload().as_slice(), &[1, 2, 3, 4, 5]);
    assert_eq!(view.payload().capacity(), 7);

    // The empty record is 3 bytes. A 256-byte growth step would take it to
    // 259 bytes, one past the maximum for its one-byte count.
    let empty = BytePayload {
        version: 7,
        payload: vec![],
    }
    .encode()
    .unwrap();
    let storage = TestStorage::new(empty);
    let capped = MaxLenStorage::new(&storage, 512, 256);
    BytePayload::decode_mut(&capped)
        .unwrap()
        .payload_mut()
        .push(42)
        .unwrap();

    let bytes = storage.bytes();
    assert_eq!(bytes.len(), BytePayload::MAX_DATA_LEN);
    let decoded =
        BytePayload::decode(&bytes).expect("a successful push must leave a readable record");
    assert_eq!(decoded.payload().as_slice(), &[42]);
    assert_eq!(decoded.payload().capacity(), u8::MAX as usize);
    let mut reopened =
        BytePayload::decode_mut(&capped).expect("the record must also reopen for mutation");
    assert_eq!(reopened.payload_mut().len(), 1);
}

#[test]
fn cap_only_wrapper_preserves_inner_growth_step_and_following_record() {
    let next = BytePayload {
        version: 9,
        payload: (10..20).collect(),
    }
    .encode()
    .unwrap();
    let mut bytes = vec![7, 0, 0];
    bytes.extend_from_slice(&next);
    let initial_len = bytes.len();
    let storage = TestStorage::new(bytes);

    // One wrapper is enough to set both the cap and the growth step.
    // An outer cap-only wrapper should preserve the inner eight-byte step.
    let cap = initial_len + 16;
    let inner = MaxLenStorage::new(&storage, cap, 8);
    let outer = MaxLenStorage::new(&inner, cap, 0);
    assert_eq!(
        BytePayload::decode_mut(&outer).unwrap_err(),
        ProgramError::from(DataLayoutError::InvalidDataLength)
    );

    // Reopen on each push. The first reserves eight slots before the next
    // record. Pushes 2–8 reuse them, and push 9 moves the record once more.
    let values = [1, 2, 3, 4, 5, 6, 7, 8, 9];
    for (index, &value) in values.iter().enumerate() {
        let mut view = BytePayload::decode_prefix_mut(&outer).unwrap();
        let mut payload = view.payload_mut();
        payload.push(value).unwrap();
        let capacity = if index < 8 { 8 } else { 16 };
        assert_eq!(payload.capacity(), capacity);
        let (first, suffix) = BytePayload::decode_prefix(view.bytes()).unwrap();
        assert_eq!(first.payload().capacity(), capacity);
        assert_eq!(first.payload().as_slice(), &values[..=index]);
        assert_eq!(first.bytes().len(), BytePayload::MIN_DATA_LEN + capacity);
        assert_eq!(suffix, next);
        assert_eq!(BytePayload::decode(suffix).unwrap().version(), 9);
    }
    assert_eq!(storage.resize_lens(), [initial_len + 8, initial_len + 16]);

    let mut view = BytePayload::decode_prefix_mut(&outer).unwrap();
    let mut field = view.payload_mut();
    assert_eq!(
        field.extend_from_slice(&[0; 8]).unwrap_err(),
        ProgramError::from(DataLayoutError::LengthExceedsCapacity)
    );
    assert_eq!((field.len(), field.capacity()), (9, 16));
    field.clear().unwrap();
    let (first, suffix) = BytePayload::decode_prefix(view.bytes()).unwrap();
    assert_eq!((first.payload().len(), first.payload().capacity()), (0, 16));
    assert_eq!(suffix, next);
    assert_eq!(storage.resize_lens(), [initial_len + 8, initial_len + 16]);
}

#[fixed_offset_layout(buffer_offset = any)]
struct FlexibleVariableNestedOptionArgs {
    header: u8,
    #[flexible]
    config: Option<BytePayload>,
}

#[test]
fn numeric_nested_option_round_trips_at_its_declared_offset() {
    #[fixed_offset_layout(buffer_offset = 2)]
    struct Child {
        value: u8,
    }
    #[fixed_offset_layout(buffer_offset = 0)]
    struct Parent {
        version: u8,
        #[flexible]
        child: Option<Child>,
    }

    let record = Parent {
        version: 7,
        child: Some(Child { value: 42 }),
    };
    let mut bytes = Aligned([0; Parent::MAX_DATA_LEN]);
    assert!(record.encode_to(&mut bytes.0).unwrap().is_empty());
    assert_eq!(bytes.0, [7, 1, 42]);
    assert_eq!(
        Parent::decode(&bytes.0).unwrap().child().unwrap().value(),
        42
    );
}

#[test]
fn fixed_offset_layout_supports_flexible_variable_nested_option() {
    assert_eq!(FlexibleVariableNestedOptionArgs::MIN_DATA_LEN, 2);
    assert_eq!(FlexibleVariableNestedOptionArgs::MAX_DATA_LEN, 260);

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
        view.config_mut().set(Some(&config)).unwrap();
    }

    assert_eq!(
        FlexibleVariableNestedOptionArgs {
            header: 7,
            config: Some(config),
        }
        .encode()
        .unwrap(),
        vec![7, 1, 3, 3, 3, 9, 8, 7],
    );

    {
        let bytes = storage.bytes();
        let mut aligned = aligned_copy::<16>(&bytes);
        let view = FlexibleVariableNestedOptionArgs::decode(&aligned.0[..bytes.len()]).unwrap();
        let config_view = view.config().unwrap();

        assert_eq!(config_view.version(), 3);
        assert_eq!(config_view.payload().as_slice(), &[9, 8, 7]);

        // Reserve two spare slots in the nested BytePayload.
        aligned.0[4] = 5;
        let (parent, rest) = FlexibleVariableNestedOptionArgs::decode_prefix(&aligned.0).unwrap();
        assert_eq!(rest, &aligned.0[bytes.len() + 2..]);
        let payload = parent.config().unwrap().payload();
        assert_eq!((payload.len(), payload.capacity()), (3, 5));
    }

    let shorter = BytePayload {
        version: 4,
        payload: vec![5],
    };
    {
        let mut view = FlexibleVariableNestedOptionArgs::decode_mut(&storage).unwrap();
        view.config_mut().set(Some(&shorter)).unwrap();
    }
    assert_eq!(storage.bytes(), vec![7, 1, 4, 1, 1, 5]);

    {
        let mut view = FlexibleVariableNestedOptionArgs::decode_mut(&storage).unwrap();
        view.config_mut().set(None).unwrap();
    }
    assert_eq!(storage.bytes(), vec![7, 0]);
    assert_eq!(storage.resize_lens(), vec![8, 6, 2]);
}

#[test]
fn fixed_offset_layout_rejects_invalid_flexible_nested_option() {
    for (bytes, expected_error) in [
        (vec![7, 2, 3, 1, 9], DataLayoutError::InvalidOptionTag),
        (vec![7, 1], DataLayoutError::TruncatedPayload),
        (
            vec![7, 1, 3, 3, 1, 9],
            DataLayoutError::LengthExceedsCapacity,
        ),
        (vec![0; 261], DataLayoutError::InvalidDataLength),
    ] {
        let aligned = aligned_copy::<264>(&bytes);
        assert_eq!(
            FlexibleVariableNestedOptionArgs::decode(&aligned.0[..bytes.len()]).unwrap_err(),
            expected_error,
        );
    }
}

// With `any`, `shuttle_id_mut()` returns `LayoutCopyMut` and we write with `.set(101)`.
// Offset 0 lets this test write through `&mut u32` and `&mut u16` because `shuttle_id`
// and `checksum` are aligned at bytes 0 and 110. The `u64` at byte 4 still uses `LayoutCopyMut`.
#[fixed_offset_layout(buffer_offset = 0)]
struct PrivateTransferFixedArgs {
    shuttle_id: u32,
    amount: u64,
    validator: Option<[u8; 32]>,
    #[capacity = 2]
    recipients: Vec<Address>,
    checksum: u16,
}

impl PrivateTransferFixedArgs {
    fn example() -> Self {
        Self {
            shuttle_id: 100,
            amount: 200,
            validator: Some([1; 32]),
            recipients: vec![Address::from([1; 32])],
            checksum: 0xBEEF,
        }
    }
}

#[test]
fn fixed_capacity_layout_validates_its_wire_format_and_prefix() {
    assert_eq!(PrivateTransferFixedArgs::DATA_LEN, 112);
    assert_eq!(PrivateTransferFixedArgs::OFFSETS, [0, 4, 12, 45, 110]);

    let mut value = PrivateTransferFixedArgs::example();
    value.recipients.push(Address::from([2; 32]));

    let mut aligned = Aligned([0; PrivateTransferFixedArgs::DATA_LEN]);
    let bytes = &mut aligned.0;

    bytes[0..4].copy_from_slice(&100_u32.to_le_bytes());
    bytes[4..12].copy_from_slice(&200_u64.to_le_bytes());
    bytes[12] = 1;
    bytes[13..45].copy_from_slice(&[1; 32]);
    bytes[45] = 2;
    bytes[46..78].copy_from_slice(&[1; 32]);
    bytes[78..110].copy_from_slice(&[2; 32]);
    bytes[110..112].copy_from_slice(&0xBEEF_u16.to_le_bytes());

    let view = PrivateTransferFixedArgs::decode(bytes).unwrap();
    assert_eq!(view.shuttle_id(), 100);
    assert_eq!(view.amount(), 200);
    assert_eq!(view.validator(), Some(&[1; 32]));
    assert_eq!(view.recipients(), value.recipients);
    assert_eq!(view.recipients_capacity(), 2);
    assert_eq!(view.checksum(), 0xBEEF);

    assert_eq!(value.encode().unwrap(), aligned.0.to_vec());

    let mut framed = Aligned([0; PrivateTransferFixedArgs::DATA_LEN + 3]);
    framed.0[PrivateTransferFixedArgs::DATA_LEN..].copy_from_slice(&[9, 8, 7]);
    assert_eq!(value.encode_to(&mut framed.0).unwrap(), &[9, 8, 7]);
    assert_eq!(
        &framed.0[..PrivateTransferFixedArgs::DATA_LEN],
        aligned.0.as_slice()
    );

    let (view, remaining) = PrivateTransferFixedArgs::decode_prefix(&framed.0).unwrap();
    assert_eq!(view.shuttle_id(), 100);
    assert_eq!(remaining, &[9, 8, 7]);

    aligned.0[PrivateTransferFixedArgs::OFFSETS[3]] = 3;
    assert_eq!(
        PrivateTransferFixedArgs::decode(&aligned.0).unwrap_err(),
        DataLayoutError::LengthExceedsCapacity
    );
}

#[test]
fn fixed_offset_layout_mutates_fixed_fields_and_fixed_capacity_vec() {
    let encoded = PrivateTransferFixedArgs::example().encode().unwrap();
    assert_eq!(&encoded[78..110], &[0; 32]);
    let storage = TestStorage::new(encoded);
    let mut view = PrivateTransferFixedArgs::decode_mut(&storage).unwrap();
    assert_eq!(view.storage_len(), PrivateTransferFixedArgs::DATA_LEN);
    {
        let read = view.as_view();
        assert_eq!(read.shuttle_id(), 100);
        assert_eq!(read.amount(), 200);
        assert_eq!(read.validator(), Some(&[1; 32]));
    }

    *view.shuttle_id_mut() = 101;
    *view.checksum_mut() = 0xCAFE;
    let mut amount = view.amount_mut();
    assert_eq!(amount.get(), 200);
    amount.set(202);
    let mut validator = view.validator_mut();
    assert_eq!(validator.get(), Some([1; 32]));
    validator.set(None);
    assert_eq!(validator.get(), None);
    validator.set(Some([3; 32]));

    let mut recipients = view.recipients_mut();
    assert_eq!((recipients.len(), recipients.capacity()), (1, 2));
    assert_eq!(recipients.get(0), Some(Address::from([1; 32])));
    recipients.push(Address::from([2; 32])).unwrap();
    assert_eq!(
        recipients.push(Address::from([3; 32])).unwrap_err(),
        ProgramError::from(DataLayoutError::LengthExceedsCapacity)
    );
    assert_eq!(recipients.pop().unwrap(), Some(Address::from([2; 32])));
    recipients.push(Address::from([3; 32])).unwrap();
    recipients.set(0, Address::from([2; 32])).unwrap();
    recipients.truncate(1).unwrap();
    recipients.push(Address::from([4; 32])).unwrap();
    {
        let read = view.as_view();
        assert_eq!(read.shuttle_id(), 101);
        assert_eq!(read.amount(), 202);
        assert_eq!(read.validator(), Some(&[3; 32]));
    }
    drop(view);

    let bytes = storage.bytes();
    assert_eq!(bytes.len(), PrivateTransferFixedArgs::DATA_LEN);
    let aligned = aligned_copy::<{ PrivateTransferFixedArgs::DATA_LEN }>(&bytes);
    let view = PrivateTransferFixedArgs::decode(&aligned.0).unwrap();
    assert_eq!(view.shuttle_id(), 101);
    assert_eq!(view.amount(), 202);
    assert_eq!(view.validator(), Some(&[3; 32]));
    assert_eq!(
        view.recipients(),
        &[Address::from([2; 32]), Address::from([4; 32])]
    );
    assert_eq!(view.checksum(), 0xCAFE);
}

#[test]
fn fixed_offset_layout_supports_unchecked_read_and_mutable_views() {
    let encoded = PrivateTransferFixedArgs::example().encode().unwrap();
    let aligned = aligned_copy::<{ PrivateTransferFixedArgs::DATA_LEN }>(&encoded);

    // SAFETY: `aligned` contains one freshly encoded record at offset zero.
    let view = unsafe { PrivateTransferFixedArgs::decode_unchecked(&aligned.0) };
    assert_eq!(view.shuttle_id(), 100);
    assert_eq!(view.amount(), 200);
    assert_eq!(view.validator(), Some(&[1; 32]));

    let storage = TestStorage::new(encoded);
    // SAFETY: `storage` contains one freshly encoded record at offset zero.
    let mut view = unsafe { PrivateTransferFixedArgs::decode_mut_unchecked(&storage).unwrap() };
    *view.shuttle_id_mut() = 101;
    view.amount_mut().set(202);
    *view.checksum_mut() = 0xCAFE;
    drop(view);

    let bytes = storage.bytes();
    let aligned = aligned_copy::<{ PrivateTransferFixedArgs::DATA_LEN }>(&bytes);
    let view = PrivateTransferFixedArgs::decode(&aligned.0).unwrap();
    assert_eq!(view.shuttle_id(), 101);
    assert_eq!(view.amount(), 202);
    assert_eq!(view.checksum(), 0xCAFE);
}

// The layout needs a known offset to borrow `tail` as `&[u16]`. Its first
// element is 10 bytes into the struct, so offset 0 gives it an even address.
#[fixed_offset_layout(buffer_offset = 0)]
struct FixedTrailingVecArgs {
    header: u8,
    #[capacity = 4]
    reserved: Vec<u8>,
    #[extendable = 2]
    tail: Vec<u16>,
}

#[test]
fn trailing_u16_vec_decodes_active_elements_and_spare_capacity() {
    assert_eq!(FixedTrailingVecArgs::MIN_DATA_LEN, 10);
    assert_eq!(FixedTrailingVecArgs::MAX_DATA_LEN, 6 + 4 + 0xFFFF * 2);
    assert_eq!(FixedTrailingVecArgs::OFFSETS, [0, 1, 6]);

    let mut value = FixedTrailingVecArgs {
        header: 7,
        reserved: vec![1, 2],
        tail: vec![0x0102, 0x0304],
    };
    let encoded = value.encode().unwrap();
    assert_eq!(encoded, [7, 2, 1, 2, 0, 0, 2, 0, 2, 0, 2, 1, 4, 3]);
    let mut aligned = aligned_copy::<22>(&encoded);
    for spare in [0, 4] {
        aligned.0[8..10].copy_from_slice(&(2 + spare as u16).to_le_bytes());
        let storage = &aligned.0[..encoded.len() + spare * 2];
        let view = FixedTrailingVecArgs::decode(storage).unwrap();
        assert_eq!(view.header(), value.header);
        assert_eq!(view.reserved(), value.reserved);
        assert_eq!(view.reserved_capacity(), 4);
        let tail = view.tail();
        assert_eq!(tail.as_slice(), value.tail);
        assert_eq!((tail.len(), tail.capacity()), (2, 2 + spare));
        assert_eq!(tail.encoded_len(), 8);
        assert_eq!(tail.storage_len(), 8 + spare * 2);
        assert_eq!(tail.storage_bytes(), &storage[6..]);
    }

    let next = BytePayload {
        version: 9,
        payload: vec![42],
    };
    for tail in [vec![], vec![0x0102, 0x0304]] {
        value.tail = tail;
        let mut framed = Aligned([0xff; 18]);
        let rest = value.encode_to(&mut framed.0).unwrap();
        let unused = next.encode_to(rest).unwrap().len();
        let (first, rest) =
            FixedTrailingVecArgs::decode_prefix(&framed.0[..framed.0.len() - unused]).unwrap();
        assert_eq!(first.bytes().len(), value.encoded_len().unwrap());
        assert_eq!(first.tail().as_slice(), value.tail);
        assert_eq!(first.tail().capacity(), value.tail.len());
        assert_eq!(rest, &[9, 1, 1, 42]);
        assert_eq!(
            BytePayload::decode(rest).unwrap().payload().as_slice(),
            &[42]
        );
    }

    for (truncated_size, expected) in [
        (6, DataLayoutError::MissingLengthHeader),
        (7, DataLayoutError::MissingLengthHeader),
        (8, DataLayoutError::MissingCapacityHeader),
        (9, DataLayoutError::MissingCapacityHeader),
        (12, DataLayoutError::TruncatedVectorPayload),
    ] {
        let bytes = &aligned.0[..truncated_size];
        assert_eq!(FixedTrailingVecArgs::decode(bytes).unwrap_err(), expected);
        assert_eq!(
            wheels::layout::decode_extendable_vec_header(
                bytes,
                FixedTrailingVecArgs::OFFSETS[2],
                2,
                size_of::<u16>(),
            ),
            Err(expected)
        );
    }

    #[fixed_offset_layout(buffer_offset = 0)]
    struct SamplesAfterConfigs {
        #[capacity = 1]
        configs: Vec<AccountConfig>,
        _pad: u8,
        #[capacity = 1]
        reserved_samples: Vec<u16>,
        _pad_counters: [u8; 2],
        counters: [u32; 4],
        #[extendable = 2]
        samples: Vec<u16>,
    }

    let bytes = Aligned([0; SamplesAfterConfigs::MIN_DATA_LEN]);
    assert_eq!(SamplesAfterConfigs::OFFSETS, [0, 38, 39, 42, 44, 60]);
    let view = SamplesAfterConfigs::decode(&bytes.0).unwrap();
    assert_eq!(view.counters(), &[0; 4]);
    assert!(view.reserved_samples().is_empty());
    assert!(view.samples().as_slice().is_empty());
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
    assert_eq!(empty, [7, 2, 1, 2, 0, 0, 0, 0, 0, 0]);
    let mut aligned = aligned_copy::<12>(&empty);
    for spare in [0, 1] {
        aligned.0[8..10].copy_from_slice(&(spare as u16).to_le_bytes());
        let view = FixedTrailingVecArgs::decode(&aligned.0[..empty.len() + spare * 2]).unwrap();
        let tail = view.tail();
        assert!(tail.as_slice().is_empty());
        assert_eq!(tail.encoded_len(), 4);
        assert_eq!(tail.capacity(), spare);
    }

    let next_record = BytePayload {
        version: 9,
        payload: vec![42],
    }
    .encode()
    .unwrap();
    let storage = TestStorage::new([empty.as_slice(), &next_record].concat());
    // The growth allowance fits the two appended u16s.
    let capped = MaxLenStorage::new(&storage, 15 + next_record.len(), 7);
    let mut view = FixedTrailingVecArgs::decode_prefix_mut(&capped).unwrap();
    let mut tail = view.tail_mut();
    assert_eq!((tail.len(), tail.capacity()), (0, 0));
    tail.extend_from_slice(&[0x0102, 0x0304]).unwrap();
    assert_eq!((tail.len(), tail.capacity()), (2, 2));
    {
        let read = view.as_view();
        assert_eq!(read.bytes().len(), 14);
        assert_eq!(read.tail().as_slice(), &[0x0102, 0x0304]);
    }
    let (record, suffix) = FixedTrailingVecArgs::decode_prefix(view.bytes()).unwrap();
    assert_eq!(record.tail().as_slice(), &[0x0102, 0x0304]);
    assert_eq!(suffix, next_record);
    assert_eq!(
        BytePayload::decode(suffix).unwrap().payload().as_slice(),
        &[42]
    );
    assert_eq!(
        view.bytes()[..14],
        [7, 2, 1, 2, 0, 0, 2, 0, 2, 0, 2, 1, 4, 3]
    );
    let mut tail = view.tail_mut();

    tail.set(1, 0x0506).unwrap();
    assert_eq!(tail.pop().unwrap(), Some(0x0506));
    tail.push(0x0708).unwrap();
    let (decoded, _) = FixedTrailingVecArgs::decode_prefix(view.bytes()).unwrap();
    assert_eq!(decoded.tail().as_slice(), &[0x0102, 0x0708]);

    view.tail_mut().clear().unwrap();
    drop(view);
    let bytes = storage.bytes();
    let (view, suffix) = FixedTrailingVecArgs::decode_prefix(&bytes).unwrap();
    assert_eq!(suffix, next_record);
    assert_eq!((view.tail().len(), view.tail().capacity()), (0, 2));
}

#[test]
fn nested_storage_caps_limit_growth_to_whole_elements() {
    for (inner_cap, inner_step, outer_cap, outer_step) in [(64, 16, 15, 0), (15, 0, 64, 16)] {
        let storage = TestStorage::new(vec![0; FixedTrailingVecArgs::MIN_DATA_LEN]);
        let inner = MaxLenStorage::new(&storage, inner_cap, inner_step);
        let outer = MaxLenStorage::new(&inner, outer_cap, outer_step);
        let mut view = FixedTrailingVecArgs::decode_mut(&outer).unwrap();
        let mut tail = view.tail_mut();

        // The u16 payload starts at byte 10. A 15-byte cap fits two elements.
        tail.push(1).unwrap();
        tail.push(2).unwrap();
        assert_eq!(
            tail.push(3).unwrap_err(),
            ProgramError::from(DataLayoutError::LengthExceedsCapacity)
        );
        assert_eq!(storage.resize_lens(), [14]);
        let decoded = FixedTrailingVecArgs::decode(view.bytes()).unwrap();
        assert_eq!(decoded.tail().as_slice(), &[1, 2]);
    }
}

#[test]
fn account_view_retains_exclusive_access_after_rejected_and_successful_growth() {
    use pinocchio::account::{
        AccountView, RuntimeAccount, MAX_PERMITTED_DATA_INCREASE, NOT_BORROWED,
    };

    #[repr(C)]
    struct AccountBuffer {
        header: RuntimeAccount,
        data: [u8; FixedTrailingVecArgs::MIN_DATA_LEN + MAX_PERMITTED_DATA_INCREASE],
    }
    let mut buffer = AccountBuffer {
        header: RuntimeAccount {
            borrow_state: NOT_BORROWED,
            is_writable: 1,
            data_len: FixedTrailingVecArgs::MIN_DATA_LEN as u64,
            ..Default::default()
        },
        data: [0; FixedTrailingVecArgs::MIN_DATA_LEN + MAX_PERMITTED_DATA_INCREASE],
    };
    // SAFETY: The allocation holds the runtime header followed by the account
    // data and its full growth allowance, and outlives the view and its borrows.
    let account = unsafe { AccountView::new_unchecked((&raw mut buffer).cast()) };
    let mut view = FixedTrailingVecArgs::decode_mut(&account).unwrap();
    let mut tail = view.tail_mut();
    assert_eq!(
        tail.extend_from_slice(&vec![0; MAX_PERMITTED_DATA_INCREASE / 2 + 1]),
        Err(ProgramError::InvalidRealloc)
    );
    assert_eq!((tail.len(), tail.capacity()), (0, 0));
    assert_eq!(
        account.check_borrow(),
        Err(ProgramError::AccountBorrowFailed)
    );

    tail.push(42).unwrap();
    *view.header_mut() = 7;
    assert_eq!(
        account.check_borrow(),
        Err(ProgramError::AccountBorrowFailed)
    );
    drop(view);

    let bytes = account.try_borrow().unwrap();
    let decoded = FixedTrailingVecArgs::decode(&bytes).unwrap();
    assert_eq!(decoded.header(), 7);
    assert_eq!(decoded.tail().as_slice(), &[42]);
}

// The layout needs an odd base offset to borrow `payload` as `&[u16]` after
// the 1-byte tag and two 8-byte headers. Offset 1 puts element 0 at byte 18.
#[fixed_offset_layout(buffer_offset = 1)]
struct FixedTrailingWideVecArgs {
    tag: u8,
    #[extendable = 8]
    payload: Vec<u16>,
}

#[test]
fn wide_length_header_round_trips_an_aligned_u16_slice() {
    const COUNT: usize = 0x1_0000;
    const FRAME_LEN: usize = 1 + 16 + COUNT * 2;

    assert_eq!(FixedTrailingWideVecArgs::MIN_DATA_LEN, 17);
    assert_eq!(
        FixedTrailingWideVecArgs::MAX_DATA_LEN,
        1 + 16 + u32::MAX as usize * 2
    );
    assert_eq!(FixedTrailingWideVecArgs::OFFSETS, [0, 1]);

    let value = FixedTrailingWideVecArgs {
        tag: 7,
        payload: (0..=u16::MAX).collect(),
    };
    let mut aligned = Aligned([0; 1 + FRAME_LEN]);
    let frame = &mut aligned.0[1..];
    assert!(value.encode_to(frame).unwrap().is_empty());
    assert_eq!(frame[0], 7);
    assert_eq!(&frame[1..9], &(COUNT as u64).to_le_bytes());
    assert_eq!(&frame[9..17], &(COUNT as u64).to_le_bytes());
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

#[fixed_offset_layout(buffer_offset = any)]
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

#[fixed_offset_layout(buffer_offset = any)]
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
        let mut entries = view.entries_mut();
        assert_eq!(entries.len(), 0);
        assert_eq!(entries.capacity(), 3);

        entries.push(&FixedEntry::new(0x0102, Some(false))).unwrap();
        entries.push(&FixedEntry::new(0x0304, Some(true))).unwrap();
        entries.push(&FixedEntry::new(0x0708, None)).unwrap();
        let full = view.bytes().to_vec();
        assert_eq!(&full[..6], &[9, 3, 0x02, 0x01, 1, 0]);
        let mut entries = view.entries_mut();
        entries.truncate(entries.len()).unwrap();
        assert_eq!(view.bytes(), full);
        let mut entries = view.entries_mut();

        entries.set(0, &FixedEntry::new(0x0506, None)).unwrap();
        entries.pop().unwrap();
        assert_eq!(entries.len(), 2);
        entries.truncate(1).unwrap();
    }

    drop(view);
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

#[test]
fn fixed_capacity_length_writes_cross_the_one_byte_boundary() {
    #[fixed_offset_layout(buffer_offset = any)]
    struct Record {
        #[capacity = 255]
        small: Vec<u8>,
        #[capacity = 256]
        large: Vec<u8>,
    }

    let record = Record {
        small: vec![0; 254],
        large: vec![0; 255],
    };
    let storage = TestStorage::new(record.encode().unwrap());
    let mut view = Record::decode_mut(&storage).unwrap();
    let full = ProgramError::from(DataLayoutError::LengthExceedsCapacity);
    let mut small = view.small_mut();
    small.push(7).unwrap();
    assert_eq!(small.push(9).unwrap_err(), full);
    let mut large = view.large_mut();
    large.push(8).unwrap();
    assert_eq!(large.push(9).unwrap_err(), full);

    let [small, large] = Record::OFFSETS;
    assert_eq!(view.bytes()[small], 255);
    assert_eq!(&view.bytes()[large..large + 2], &[0, 1]);
    let decoded = Record::decode(view.bytes()).unwrap();
    assert_eq!(decoded.small()[254], 7);
    assert_eq!(decoded.large()[255], 8);
}

#[fixed_offset_layout(buffer_offset = any)]
struct FixedTrailingEntryVecArgs {
    tag: u8,
    #[extendable = 1]
    entries: Vec<FixedEntry>,
}

#[test]
fn trailing_entry_vec_round_trips_through_storage() {
    assert_eq!(FixedTrailingEntryVecArgs::MIN_DATA_LEN, 3);
    assert_eq!(
        FixedTrailingEntryVecArgs::MAX_DATA_LEN,
        1 + 2 + 0xFF * FixedEntry::DATA_LEN
    );
    assert_eq!(FixedTrailingEntryVecArgs::OFFSETS, [0, 1]);

    let mut value = FixedTrailingEntryVecArgs {
        tag: 7,
        entries: vec![],
    };
    let empty = value.encode().unwrap();
    assert_eq!(empty, [7, 0, 0]);
    let aligned = aligned_copy::<3>(&empty);
    assert!(FixedTrailingEntryVecArgs::decode(&aligned.0)
        .unwrap()
        .entries()
        .is_empty());

    let storage = TestStorage::new(empty);
    let mut view = FixedTrailingEntryVecArgs::decode_mut(&storage).unwrap();
    let mut entries = view.entries_mut();
    assert_eq!((entries.len(), entries.capacity()), (0, 0));
    value.entries = vec![
        FixedEntry::new(0x0102, Some(false)),
        FixedEntry::new(0x0304, Some(true)),
    ];
    for entry in &value.entries {
        entries.push(entry).unwrap();
    }
    let encoded = value.encode().unwrap();
    assert_eq!(encoded, [7, 2, 2, 2, 1, 1, 0, 4, 3, 1, 1]);
    assert_eq!(view.bytes(), encoded);
    let aligned = aligned_copy::<11>(&encoded);
    let decoded = FixedTrailingEntryVecArgs::decode(&aligned.0).unwrap();
    assert_eq!(decoded.entries().len(), 2);
    for (actual, expected) in decoded.entries().iter().zip(&value.entries) {
        assert_eq!(actual.id(), expected.id);
        assert_eq!(actual.enabled(), expected.enabled);
    }

    let mut entries = view.entries_mut();
    entries.set(0, &FixedEntry::new(0x0506, None)).unwrap();
    entries.pop().unwrap();
    assert_eq!((entries.len(), entries.capacity()), (1, 2));
    drop(view);
    let bytes = storage.bytes();
    let aligned = aligned_copy::<11>(&bytes);
    let view = FixedTrailingEntryVecArgs::decode(&aligned.0[..bytes.len()]).unwrap();
    let entries = view.entries();
    assert_eq!((entries.len(), entries.capacity()), (1, 2));
    let first = entries.get(0).unwrap();
    assert_eq!(first.id(), 0x0506);
    assert_eq!(first.enabled(), None);
}

#[test]
fn fixed_offset_layout_rejects_non_element_sized_trailing_storage() {
    let storage = [7, 0, 0, 0].as_slice();
    let aligned = aligned_copy::<64>(storage);

    assert_eq!(
        FixedTrailingEntryVecArgs::decode(&aligned.0[..storage.len()]).unwrap_err(),
        DataLayoutError::InvalidDataLength
    );
}
