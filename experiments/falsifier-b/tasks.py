"""Falsifier B task set.

Five small, fully specified, self-contained Rust items. No repo context, no
external crates. Tests live in tests_ref/<id>.rs and are written by the rig,
never by the model.
"""

COMMON = """
Output rules (follow exactly):
- Reply with ONE Rust code block and nothing else. No prose before or after it.
- Rust edition 2021, `std` only, no external crates.
- Use exactly the signatures given. Every item you define must be `pub`.
- Do not write tests, a `main`, or a `mod`.
"""

TASKS = [
{
 "id": "merge_intervals",
 "prompt": """Implement this Rust function.

```rust
pub fn merge_intervals(intervals: &[(i64, i64)]) -> Vec<(i64, i64)>
```

Each input tuple `(start, end)` is a closed interval; you may assume `start <= end`.

Behaviour:
- Return the intervals merged and sorted ascending by start.
- Two intervals merge if they overlap OR touch: `(1,4)` and `(4,5)` merge into `(1,5)`.
- Intervals with a gap do not merge: `(1,4)` and `(5,6)` stay separate.
- A fully nested interval is absorbed: `(1,10)` and `(2,3)` merge into `(1,10)`.
- The input may be in any order and may contain duplicates.
- Empty input returns an empty `Vec`.
- Values may be negative.
- Do not mutate through the slice; return a new `Vec`.
""" + COMMON,
},
{
 "id": "parse_size",
 "prompt": """Implement this Rust function.

```rust
pub fn parse_size(s: &str) -> Option<u64>
```

It parses a byte-size string into a number of bytes.

Grammar: optional surrounding ASCII whitespace, then a non-negative decimal
integer (ASCII digits only), then optional ASCII whitespace, then an optional
unit suffix, then optional trailing ASCII whitespace.

Unit suffixes, matched case-insensitively:
- absent, or `B` -> multiplier 1
- `KiB`, `MiB`, `GiB`, `TiB` -> 1024, 1024^2, 1024^3, 1024^4
- `KB`, `MB`, `GB`, `TB` -> 1000, 1000^2, 1000^3, 1000^4

Return `Some(bytes)` on success. Return `None` if:
- the string is empty or whitespace only,
- there are no digits,
- there is a sign, a decimal point, or any other stray character,
- the suffix is not one of the above,
- there is trailing junk after the suffix,
- the digits do not fit in a `u64`, or the multiplication overflows `u64`.

Examples: `"512"` -> 512, `"512B"` -> 512, `"4KiB"` -> 4096, `"4 KiB"` -> 4096,
`"  4kib  "` -> 4096, `"1MB"` -> 1000000, `"1MiB"` -> 1048576, `"4Kb"` -> 4000,
`"4.5KiB"` -> None, `"-1"` -> None, `"4KB junk"` -> None.
""" + COMMON,
},
{
 "id": "longest_common_prefix",
 "prompt": """Implement this Rust function.

```rust
pub fn longest_common_prefix(a: &[u32], b: &[u32]) -> usize
```

Return the number of leading elements that `a` and `b` have in common: the
largest `n` such that `a[..n] == b[..n]`.

- If either slice is empty, return 0.
- If one slice is a prefix of the other, return the shorter length.
- It must not panic for any input, including slices of different lengths.
""" + COMMON,
},
{
 "id": "lru_cache",
 "prompt": """Implement a bounded LRU cache in Rust with exactly this public API.

```rust
pub struct LruCache { /* your fields */ }

impl LruCache {
    pub fn new(capacity: usize) -> Self;
    pub fn get(&mut self, key: u64) -> Option<String>;
    pub fn put(&mut self, key: u64, value: String);
    pub fn len(&self) -> usize;
}
```

Behaviour:
- The cache holds at most `capacity` entries.
- `get` returns a clone of the stored value if the key is present, and marks
  that key as most-recently-used. On a miss it returns `None` and must NOT
  change the recency order of anything.
- `put` inserts a new entry or overwrites an existing one; either way the key
  becomes most-recently-used. Overwriting an existing key must not change the
  number of entries.
- If inserting a NEW key would exceed `capacity`, evict the least-recently-used
  entry first.
- `capacity == 0` means the cache stores nothing: `put` is a no-op, `len()`
  stays 0, `get` always returns `None`.
- `len` returns the current number of entries.
- Only `std`. Correctness matters, asymptotics do not.
""" + COMMON,
},
{
 "id": "parse_range_list",
 "prompt": """Implement this Rust function.

```rust
pub fn parse_range_list(s: &str) -> Option<Vec<u32>>
```

It parses a comma-separated list of numbers and inclusive ranges, e.g.
`"1-3,5,7-8"` -> `[1, 2, 3, 5, 7, 8]`.

Rules:
- Items are separated by `,`. Each item is either a single non-negative decimal
  integer `N`, or a range `A-B` where `A <= B`, both non-negative decimal
  integers.
- Ranges expand ascending and inclusive. `"3-3"` -> `[3]`.
- Output preserves the order the items were written in. Do NOT sort and do NOT
  deduplicate: `"2,1"` -> `[2, 1]`, `"1-2,1-2"` -> `[1, 2, 1, 2]`.
- ASCII whitespace around items and around the numbers is allowed and ignored:
  `" 1 - 3 , 5 "` -> `[1, 2, 3, 5]`.
- An empty or whitespace-only input returns `Some(vec![])`.
- Return `None` if any item is empty (`"1,,2"`, `"1,2,"`), if an item is not
  well formed (`"a"`, `"1-"`, `"-1"`, `"1-2-3"`), if a range has `A > B`
  (`"3-1"`), or if a number does not fit in `u32` (`"4294967296"`).
""" + COMMON,
},
]

TASK_IDS = [t["id"] for t in TASKS]
BY_ID = {t["id"]: t for t in TASKS}
