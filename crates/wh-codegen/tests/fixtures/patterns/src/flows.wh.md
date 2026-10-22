---
module: flows
---
# Flow combinators

An order line.

```wh
struct Line:
    sku: str
    qty: u32
    price: int
```

The steps the flows combine.

```wh
def add_line(total: int, line: Line) -> int:
    ensures result == total + line.qty * line.price

def expand(line: Line) -> list[str]:
    ensures len(result) == line.qty
```

Totals by folding, units by flat-mapping, both at once in parallel.

```wh
flow order_total(lines: list[Line]) -> int:
    lines |> fold(0, add_line)

flow units(lines: list[Line]) -> list[str]:
    lines |> flat_map(expand)

flow checkout(lines: list[Line]) -> tuple[int, list[str]]:
    t, u = parallel(order_total(lines), units(lines))
    return (t, u)
```

A payment gateway that may fail, and an adapter that fails a given number
of times first.

```wh
@error
enum PayError:
    Declined
    Timeout

capability Gateway:
    def charge(amount: int) -> str raises PayError

adapter FlakyGateway implements Gateway:
    failures: int = 2
    pattern InMemory
```

Paying retries a failed charge; paying twice runs both payments in parallel.

```wh
def charge_card(amount: int) -> str raises PayError uses Gateway:
    note "delegate to Gateway.charge"

flow pay(amount: int) -> str raises PayError uses Gateway:
    amount |> retry(charge_card, times=3, backoff=1ms)

flow pay_once(amount: int) -> str raises PayError uses Gateway:
    amount |> retry(charge_card, times=2)

flow pay_both(a: int, b: int) -> tuple[str, str] raises PayError uses Gateway:
    x, y = parallel(pay(a), pay(b))
    return (x, y)
```

```wh
test "flows combine":
    expect order_total([Line(sku="a", qty=2, price=3), Line(sku="b", qty=1, price=4)]) == 10
    expect units([Line(sku="a", qty=2, price=3)]) == ["a", "a"]
    expect checkout([Line(sku="a", qty=2, price=3)]) == (6, ["a", "a"])

test "retry until it works":
    given Gateway = FlakyGateway(failures=2)
    expect pay(5) == "paid 5"
    expect pay_both(1, 2) == ("paid 1", "paid 2")

test "retry gives up":
    given Gateway = FlakyGateway(failures=5)
    expect pay_once(5) raises PayError
```
