# Roadmap

## Milestone 0 - foundation (this archive)

- versioned configuration;
- normalized event protocol;
- matching, conditions, action registry;
- live reload and CLI simulation.

## Milestone 1 - touchpad proxy

- [x] read-only evdev discovery and device selection;
- [x] raw diagnostic observer with JSON Lines output;
- [x] experimental uinput touchpad clone and fail-open cleanup;
- [x] raw multitouch frame recorder/replayer;
- [x] configurable three-finger swipe and hold recognizers;
- [x] backward-compatible generic N-finger swipe and hold rule lists;
- [x] continuous hold-then-drag recognition lifecycle;
- tap, pinch, and rotate recognizers;
- [x] guarded uinput pointer provider for drag lifecycles;
- fail-safe uinput keyboard provider;
- [x] initial one/two-finger passthrough with three-or-more-finger interception;
- generalized passthrough policy for arbitrary unclaimed events.

## Milestone 2 - useful actions and context

- virtual keyboard and pointer actions;
- D-Bus action provider;
- GNOME adapter for overview, workspaces, and window context;
- app-specific bindings and profiles;
- continuous actions such as volume, brightness, and zoom.

## Milestone 3 - desktop application

- GTK4/libadwaita configuration UI;
- live gesture visualizer and recorder;
- conflict analysis;
- import/export and automatic configuration migrations.

## Milestone 4 - packaging and extension API

- Fedora RPM and COPR;
- stable plugin process protocol;
- signed release artifacts;
- optional adapters for other compositors and devices.
