---
'zero-rust': minor
---

Add `Zero::add_mutation_outcome_listener`, the Swift client's `addMutationOutcomeListener`, which JS has no counterpart for. The listener hears each mutation's server outcome once, as a `client::MutationServerOutcome` with the mutation's ID, name, arguments, and `MutationOutcome`, and the call returns the function that removes it. Unlike a mutation's `server` result, which settles with a `ZeroError` when the client gives up connecting (`disconnect_timeout_ms`), it hears the real outcome of a mutation replayed after the client reconnects, so an app can show each rejection in one place. Listeners stay with the `Zero` when it replaces its client; a mutation still pending when the app quits is replayed on the next launch, but its outcome isn't reported.
