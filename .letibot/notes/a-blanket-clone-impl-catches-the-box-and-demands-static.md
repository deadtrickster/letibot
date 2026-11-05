<!-- abstract: A blanket `impl<S: Any + Clone + Send + Sync> Cloneable for S` also matches `&Box<dyn Cloneable>`, so a method call on the box resolves to the REFERENCE's impl and demands `'1: 'static`; `as_deref()` naming `&dyn Cloneable` is the fix, and `dyn Any` alone cannot be cloned at all. -->

# A blanket `Cloneable` impl catches a `&Box<dyn Cloneable>` receiver, and the error says `'static`

MEASURED 2026-11-05, `crates/dialect/src/lib.rs` (worktree `wt-z-t10`): making an opaque
`Box<dyn Any + Send + Sync>` token cloneable.

`Box<dyn Any>` cannot be cloned, so the idiom is a second trait with a blanket impl:

```rust
trait Cloneable: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
    fn copied(&self) -> Box<dyn Cloneable>;
}
impl<S: std::any::Any + Clone + Send + Sync> Cloneable for S { /* ... */ }
```

Writing the call the obvious way fails with errors that name neither the blanket impl nor the box:

```
error[E0521]: borrowed data escapes outside of method
  --> crates/dialect/src/lib.rs:56:45
   |     fn clone(&self) -> RenderState {
   |              ----- `self` is a reference that is only valid in the method body
   |         RenderState(self.0.as_ref().map(|b| b.copied()))
   |                                             ^^^^^^^^^^ argument requires that `'1` must outlive `'static`
error[E0515]: cannot return value referencing temporary value
```

The cause is the blanket impl matching the REFERENCE: `&Box<dyn Cloneable>` is itself
`Any + Clone + Send + Sync` when the reference is `'static`, so autoref/autoderef picks
`<&Box<..> as Cloneable>::copied`, whose `Self` is the reference and which therefore wants
`'1: 'static`. The fix is to name the trait object, so no impl for a reference can be chosen:

```rust
RenderState(self.0.as_deref().map(|b: &dyn Cloneable| b.copied()))
// and
let held: &dyn Cloneable = self.0.as_deref()?;
held.as_any().downcast_ref::<S>()
```

`Option::as_deref()` on an `Option<Box<T>>` is what makes the borrow `&T` (`&dyn Cloneable`) rather
than `&Box<T>`. When a blanket impl is in the crate, every method call on the box needs the trait
object named, not just the ones that errored.
