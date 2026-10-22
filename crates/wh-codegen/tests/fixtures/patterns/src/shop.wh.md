---
module: shop
---
# Patterns fixture

Exercises the code generated for state machines and for the pattern catalog.

## A door

What can happen to a door.

```wh
enum DoorEvent:
    Open
    Close
    Lock(str)
    Unlock(str)
```

What a door reports.

```wh
enum DoorEffect:
    Unlocked
    Alarm
```

A door with a code lock. A wrong code keeps it locked and raises the alarm.

```wh
@pattern(StateMachine)
machine Door(event: DoorEvent) -> DoorEffect:
    Open           on Close                  -> Closed
    Closed         on Open                   -> Open
    Closed         on Lock(code)             -> Locked(code)
    Locked(code)   on Unlock(c) if c == code -> Closed      emit Unlocked
    Locked(code)   on Unlock(c)              -> Locked(code) emit Alarm
    ignore Open on Open
    ignore Open on Lock
    ignore Open on Unlock
    ignore Closed on Close
    ignore Closed on Unlock
    ignore Locked on _
    initial Closed
```

Whether a door is locked.

```wh
def is_locked(d: Door) -> bool:
    ensures result == (d is Locked)
    example is_locked(Door.Locked("1"))
    example not is_locked(Door.initial())
    example Door.step(Door.Closed, DoorEvent.Lock("12")) == (Door.Locked("12"), None)
    example Door.step(Door.Locked("12"), DoorEvent.Unlock("12")) == (Door.Closed, DoorEffect.Unlocked)
    example Door.step(Door.Locked("12"), DoorEvent.Unlock("21")) == (Door.Locked("12"), DoorEffect.Alarm)
    example Door.step(Door.Open, DoorEvent.Lock("12")) == (Door.Open, None)
```

## Orders (builder)

A stock keeping unit and a quantity.

```wh
type Sku = str where matches(self, r"^[A-Z]{3}-[0-9]{3}$")
type Qty = u32 where 1 <= self <= 1000
```

An order, built field by field.

```wh
@pattern(Builder)
struct Order:
    key id: u64
    item: Sku
    qty: Qty = 1
    note: str = ""
    invariant self.qty <= 100 or self.note != ""
```

The builder fills in defaults and checks invariants.

```wh
def default_order(item: Sku) -> Order:
    example default_order("ABC-123").qty == 1
    example default_order("ABC-123").note == ""

def incomplete_order_is_rejected() -> bool:
    example incomplete_order_is_rejected()

def big_order_needs_a_note() -> bool:
    example big_order_needs_a_note()
```

## Memoized arithmetic

Naive recursion, made linear by the cache.

```wh
def fib(n: u32) -> u64:
    requires n <= 90
    example fib(10) == 55
    example fib(90) == 2880067194370816120
    property grows(n: u8):
        fib(n + 1) >= fib(n)
    pattern Memoize
```

## A repository

Items are stored by their id.

```wh
type Name = str where len(self) >= 1

struct Item:
    key id: u64
    name: Name

@error
enum StoreError:
    Missing
    Full
```

The store of items. Its contracts hold for every adapter.

```wh
@pattern(Repository[Item])
capability Items:
    def add(own item: Item) -> unit raises StoreError
    def get(id: u64) -> Item | None raises StoreError
    def count() -> int:
        ensures result >= 0
    def rename(id: u64, name: Name) -> unit raises StoreError:
        raises StoreError.Missing when id == 0
```

Items in memory, and items in memory with a capacity.

```wh
adapter MemoryItems implements Items:
    pattern InMemory

@pattern(Adapter)
adapter CappedItems implements Items:
    capacity: int = 10
```

## A playing field (dependent strategies)

A ball on a field, and a score bounded by the target.

```wh
struct Field:
    width: u16
    height: u16
    x: float
    y: float
    left: u8
    target: u8
    invariant 0.0 <= self.x < self.width and 0.0 <= self.y < self.height
    invariant self.left <= self.target
```

The area of a field. Random fields satisfy the invariants, so every field
has room for the ball.

```wh
def area(f: Field) -> int:
    ensures result == f.width * f.height
    property has_room(f: Field):
        area(f) >= 1
    pattern Transform
```

## Ranges (checked deserialization)

A range read from JSON must still be a range.

```wh
@derive(Serialize, Deserialize)
struct Span:
    lo: int
    hi: int
    invariant self.lo <= self.hi

@error
enum ParseError:
    Bad(str)

def parse_span(text: str) -> Span raises ParseError:
    example parse_span("{\"lo\": 1, \"hi\": 2}").hi == 2
    example parse_span("{\"lo\": 3, \"hi\": 2}") raises ParseError

depends serde_json = "1"
```

## Secrets

A token is secret: it never shows up in logs, traces or `Debug` output.

```wh
@sensitive
type Token = str where len(self) >= 8
```

How a token prints in `Debug`, and a login check whose password is never traced.

```wh
def token_debug(t: Token) -> str:
    example token_debug("abcdefgh") == "<redacted>"

def login(user: str, sensitive password: str) -> bool:
    example login("ann", "hunter22")
    example not login("ann", "nope")
```
