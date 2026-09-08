# Vigie

Pronounced *vee-zhee*. French for the lookout in a ship's crow's nest: the one whose
only job is to watch and call out what's coming.

Vigie is a native macOS app that watches GitHub repositories for what your team is
doing: commits, pull requests, reviews, and comments, for a chosen set of people or
everyone, with desktop notifications. No local clone required.

Architecture: a Rust engine (`crates/gitmon`) behind a Tauri 2 shell (`app/`). See
`docs/CONTRACT.md` for the interface both sides code against and `docs/design/` for
the agreed screens. The repository keeps its working name, `git-monitor`.
