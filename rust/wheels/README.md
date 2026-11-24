# wheels

`wheels` gives Solana programs typed views over account and instruction bytes,
plus macros for checking program inputs. It supports `no_std`.

## Layouts

**Fixed offsets** work well for account data. This poll reserves space for four
choices, so `winner` and `closed` stay at the same byte offsets as choices are added.

```rust
use wheels::{fixed_offset_layout, layout::{Decodable, Encodable}};

#[fixed_offset_layout(buffer_offset = any)]
struct Poll {
    #[capacity = 4]
    choices: Vec<u8>,
    winner: Option<u8>,
    closed: bool,
}

let poll = Poll { choices: vec![7, 9], winner: Some(9), closed: true };
let bytes = poll.encode().unwrap();
let view = Poll::decode(&bytes).unwrap();
assert_eq!(view.choices(), &[7, 9]);
assert_eq!(view.winner(), Some(9));
assert!(view.closed());
```

**Variable offsets** work well for compact instruction data. Here the ranking
has a one-byte count, and `weight` follows the last choice. Both examples decode
directly from the bytes without allocating.

```rust
use wheels::{layout::Decodable, variable_offset_layout};

#[variable_offset_layout(buffer_offset = any)]
struct Ballot {
    voter_id: Option<u8>,
    #[flexible = 1]
    ranking: Vec<u8>,
    weight: u8,
}

// Anonymous ballot, two choices (7 and 9), weight 3.
let ballot = Ballot::decode(&[0, 2, 7, 9, 3]).unwrap();
assert_eq!(ballot.voter_id(), None);
assert_eq!(ballot.ranking(), &[7, 9]);
assert_eq!(ballot.weight(), 3);
```

The layout macros use `bytemuck`, `pinocchio`, and `pinocchio-log` as direct
dependencies of the program. [Read more about layouts on docs.rs.](https://docs.rs/wheels/latest/wheels/layout/)

## Require macros

Return a program error if a ballot ranks too many choices or has no first choice.

```rust
use pinocchio::error::ProgramError;
use wheels::{require_le, require_some};

fn first_choice(ranking: &[u8], max_choices: usize) -> Result<u8, ProgramError> {
    require_le!(ranking.len(), max_choices, ProgramError::InvalidInstructionData);
    let choice = require_some!(ranking.first(), ProgramError::InvalidInstructionData);
    Ok(*choice)
}
```

[See the other require macros on docs.rs.](https://docs.rs/wheels/latest/wheels/requires/)
