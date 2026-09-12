## TSF re-entrancy safety

- **Treat every separately re-entrant COM call as its own authority boundary.**
  A lease check around an aggregate operation is insufficient when that
  operation performs several host calls. Check authority before and after each
  call, and suppress all later calls as soon as lifecycle, focus, context, or
  operation ownership changes.

- **Install cleanup ownership before validating authority after a successful
  host Begin.** Re-entry can invalidate the caller while `BeginUIElement`
  succeeds. Record the exact manager, element, and id first, then evaluate the
  post-call lease; otherwise teardown loses the only matching `EndUIElement`.

- **A refused single-threaded state borrow needs an out-of-band terminal
  owner.** For delayed TSF callbacks, retain an exact operation token plus one
  bounded deferred or lifecycle-owned settlement. Never leave a state such as
  `QueryQueued` merely because a `RefCell` borrow or hidden-window post failed.

- **Candidate teardown and candidate Begin/Update must share one exclusion
  domain.** Hold it across subscription removal, `UnadviseSink`,
  `EndUIElement`, controller restoration, and retained-work repost. Re-entrant
  work must survive the old teardown and keep its own eventual cleanup owner.
