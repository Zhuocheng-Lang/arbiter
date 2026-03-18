# Changelog

All notable changes to Arbiter will be documented in this file.

## Unreleased

### Changed

- Clarified documentation for `ionice` handling: it is mapped to cgroup v2 `io.weight` when a cgroup target is present.
- Clarified that `.cgroups` files are ignored and are not part of the current rule model.
- Removed the obsolete MVP specification document from the repository.
