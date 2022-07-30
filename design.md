# Design Doc

We want a way to control lights. We want those lights to respond to things
like music that is playing on our speakers, weather in some cities, the
time of day, etc. while also being manually controllable.

We also want the lights to be partially automated in selecting which
stimuli to focus on. And we only want one stimuli to have access at a time.

For example: if music is playing we want the lights to automatically respond
to the music. Otherwise if the time is past 10 pm we want the lights to turn
off. However if the user has manually selected a setting, stay on that setting
indefinitely.

The primary design goal, as it relates to code, is that we want it to be
very easy to create new things for the lights to respond to. This means
the code needs to be generic over the stimuli.

## Deeper exploration

Since the implementation will have to be completely generic over the backing stimuli we will have to define an interface over which the lights can communicate with them.

First we would need a way of knowing whether a stimuli is active and can do anything.
Without this we may allow the stimuli to control the lights even when they aren't actually going to do anything with them.

For example, the music system would need a way of telling us the music was playing.
Also some systems can always be active, like the weather display.

We would also need a way of knowing when systems no longer need access.
For example when the music stops we should stop showing the music lights.

There should be both a pull and push method of retrieving this information.
We want the lights to immediately switch to music if music starts playing but we also want the lights to switch to music if some other system with higher precedence to music gets turned off while the music is still playing.

Secondly we would need a way to decide which system should actually be able to display on the lights.
This access must be exclusive to prevent flickering.
We also want the entire system to be robust enough that it can't break and get confused about who has access.
Some system should always have access.

There is also another complication: how do you revoke access from a system?
If music is playing but the user has manually selected the off system, then the music should stop having control over the lights and it should immediately switch to being off.
Because of this we would need a way to revoke access after it is given, preferrably quickly to remain responsive.

## Proposals

### Proposal 1

In this proposal we have one central component called the broker.

The broker is responsible for fielding requests for access and it is the only thing that actually controls the lights.
All the systems control the lights through the broker.
The broker ensures that only one system can access the lights at a time and allows access to be revoked quickly.

To signal that it is active, a system sends a message to the broker indicating as such.
The broker stores this and if access is possible, it sends the system a channel to start sending colors through.
To revoke access the broker simply stops listening on the channel and closes it to indicate to the system that the broker is no longer listening.

One problem with this proposal is that, at any point after saying it's active, a system can receive access from the broker.
This means the systems have to constantly listen for possible offers from the broker making their code more complex.

Imagine the music handling code: it has to listen for the stopping of the music _and_ for access from the broker at the same time.
This is achievable through something like crossbeam::select! but it is less than ideal due to the complexity.

Here's a mockup:

```rust
enum BrokerBoundMessage {
    Active,
    Inactive
}

struct BrokerHandle {
    chan: mpsc::Sender<BrokerBoundMessage>
}

impl BrokerHandle {
    fn notify_active(&mut self)
    fn notify_inactive(&mut self)
}

enum SystemBoundMessage {
    GrantAccess(Lights)
}

struct SystemHandle {
    chan: mpsc::Sender<SystemBoundMessage>
}

impl SystemHandle {
    fn grant_access(&mut self, lights: Lights) { ... }
    fn shutdown(&mut self) { ... }
}
```

### Proposal 2

Similarly to proposal 1 there is a broker, however there is no explicit communication between the broker and the systems.
Instead we handle this the same way your operating system handles UI: each system is constantly updating it's appearance, the broker just decides who actually gets shown.

For example, if the music starts, the music system starts spitting out colors to set the lights to.
The broker sees this, but ignores it if there is some other system already displaying colors.

If the broker sees colors coming in from a system then it knows that the system is active and wants access.

This can lead to some wasted work because systems are computing what they want to display, even when it's not actually displayed.
In embedded environments (like where this is expected to run) this could be prohibitive.

Another downside is that this may not work very well for systems that update rarely, like any system that just displays a constant color.

Here's a mockup:

```rust
enum BrokerBoundMessage {
    SetColor(Color)
}

struct BrokerHandle {
    chan: mpsc::Sender<BrokerBoundMessage>
}
```

### Proposal 3

Extremely similarly to proposal 2, this proposal just adds a separate signal that can be used to determine whether a system is active or inactive aside from whether they are actively sending colors.
The broker stores the activity state for each system and is able to determine which one should be given access based upon that, instead of needing to see constant output through the stream.
The broker also stores the last seen color for each system.

This removes the downside of systems with rare updates.
However the wasted work downside still remains.

Here's a mockup:

```rust
enum BrokerBoundMessage{
    Active,
    Inactive,
    SetColor(Color),
}

struct BrokerHandle {
    chan: mpsc::Sender<BrokerBoundMessage>
}
```

### Proposal 4

Again similarly to how operating systems do UI, we have a constantly ticking display loop.
Each tick we check each system, to see whether it's active.
Depending on the activity of each system we then run a tick of that systems code by calling a predefined function.
Non-selected systems don't get ticks.

The actual computation done on a tick is controlled by the system, so they can short circuit the tick if there is nothing to be done.

This does limit systems to doing their computation within the time required for a tick, so things like networking would have to be done on another thread and would need to be done with no knowledge of whether the system is actually being displayed or not.

Here's a mockup:

```rust
trait System {
    fn is_active(&self) -> bool;
    fn tick(&mut self) -> Option<Color>;
}
```

## Recommendation

I am partial to proposal 4 right now. It seems like it solves all the problems with the simplest implementation.