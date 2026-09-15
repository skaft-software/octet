# Presentation ownership full qualification

The ownership fixture is a crate-unit test module so it exercises the private
`presentation` facade without adding SDK API. Its extracted child modules resolve
from `src/presentation.rs`.

Run the focused qualification with:

```sh
cargo test -p octet-coding-agent --lib presentation::ownership_tests
```
