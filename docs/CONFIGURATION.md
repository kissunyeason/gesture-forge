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
