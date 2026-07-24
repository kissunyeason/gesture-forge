# Configuration reference

The configuration is versioned. Version 1 contains runtime settings, security policy, and an ordered set of bindings.

## Binding

```toml
[[bindings]]
id = "unique-name"
enabled = true
priority = 100
consume = false
```

Higher priorities are evaluated first. `consume = true` prevents lower-priority matching bindings from running.

## Trigger

```toml
[bindings.trigger]
family = "touchpad.swipe"
phases = ["end"]
fingers = [3, 4]
directions = ["up"]

[bindings.trigger.min_values]
distance = 100.0

[bindings.trigger.max_values]
duration_ms = 450.0
```

An empty list means “any value”. A family may use a namespace wildcard such as `touchpad.*`.

## Conditions

Conditions are provider-defined and may be negated:

```toml
[[bindings.conditions]]
provider = "core"
condition = "app-id"
negate = false

[bindings.conditions.params]
value = "org.mozilla.firefox"
```

## Actions

Actions are provider-defined. GestureForge core does not know that a swipe should open an overview, press a key, run a program, or move a pointer.

```toml
[[bindings.actions]]
provider = "core"
action = "log"
on_error = "continue"

[bindings.actions.params]
message = "hello"
level = "info"
```

Supported error policies are `continue`, `stop-binding`, and `stop-dispatch`.

## Continuous virtual pointer drag

The uinput provider is opt-in:

```toml
[security]
allow_uinput_actions = true
```

Bind every drag lifecycle phase to the same action. An empty `phases` list
matches begin, update, end, and cancel; omitting release phases can leave a
button pressed until the provider's emergency cleanup runs.

```toml
[[bindings]]
id = "three-finger-pointer-drag"
enabled = true
priority = 200
consume = true

[bindings.trigger]
family = "touchpad.drag"
phases = []
fingers = [3]

[bindings.trigger.labels]
"recognition.rule_id" = "three-finger-drag"

[[bindings.actions]]
provider = "uinput"
action = "drag"
on_error = "stop-dispatch"

[bindings.actions.params]
button = "left"
scale = 0.5
max_delta = 200
```

Supported buttons are `left`, `middle`, and `right`. `scale` converts touchpad
coordinate deltas into relative pointer movement. `max_delta` limits a single
emitted axis step. The provider creates `/dev/uinput` lazily, attempts emergency
release after emission errors, and releases all virtual buttons when dropped.

Continuous drag events include a stable `recognition.stream_id`. Duplicate
events are idempotent, updates from a different stream are rejected, and stale
`end`/`cancel` events cannot release the active stream. If a live dispatch client
disconnects unexpectedly, the daemon synthesizes a matching cancel event.


## Virtual keyboard chord

The same `allow_uinput_actions` permission enables `uinput.key-chord`:

```toml
[[bindings]]
id = "three-finger-swipe-left"
enabled = true
priority = 190
consume = true

[bindings.trigger]
family = "touchpad.swipe"
phases = ["end"]
fingers = [3]
directions = ["left"]

[[bindings.actions]]
provider = "uinput"
action = "key-chord"
on_error = "stop-dispatch"

[bindings.actions.params]
keys = ["KEY_LEFTMETA", "KEY_PAGEDOWN"]
```

`keys` accepts one to eight distinct Linux `KEY_*` names whose numeric codes
are below the button range. The provider presses keys in listed order, releases
them in reverse order, creates the virtual keyboard lazily, and attempts an
emergency release after output failures and when the provider is dropped.

For controlled testing, run live recognition with both `--exclusive` and
`--dispatch`. Shared mode still allows the compositor to process the physical
touchpad while GestureForge injects virtual pointer motion.

The security flags are applied again during configuration reload. The daemon
rebuilds the action registry instead of retaining a provider that a newly loaded
configuration has disabled. If a stricter reload still contains a binding for a
disabled provider, the restriction is applied immediately and that binding fails
closed until it is removed or the provider is explicitly re-enabled. A failed
reload may reduce permissions, but it never grants a permission that was not
already active; permission increases require full provider validation.
