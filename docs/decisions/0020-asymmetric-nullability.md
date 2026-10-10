# ADR 0020: Asymmetric nullability between reads and writes

## Status

Accepted (2026-10-10).

## Context

Some Dokploy fields have different read and write contracts. For example,
`organization.active` returns `logo: null` for a new organization, while
`organization.update` accepts an omitted logo or a string and rejects an explicit null.

The spec's `nullable` flag previously described both directions. Marking the logo nullable made
an invalid clear operation configurable. Marking it non-nullable made discovery reject the valid
response from a fresh Dokploy instance.

## Decision

Add `read_nullable` to the field grammar. It permits a null remote observation without permitting
the document to own null. When a full write group resends current values, the request builder omits
an unmanaged remote null unless the field is writable as null through `nullable`.

`nullable` continues to mean that null is valid in the document and clears the remote value.
Import omits a `read_nullable` null because the document cannot reproduce it as an owned value.

## Consequences

- Discovery accepts response values that are valid on reads but invalid on writes.
- Documents fail validation before apply when they try to clear such a field with null.
- Full updates do not copy an invalid unmanaged null into the request body.
