---
module: stock
---
# Stock

Another module's adapter of [[shop.Items]] is held to the same contract tests.

```wh
from shop import Items

adapter StockItems implements Items:
    pattern InMemory
```
