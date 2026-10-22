---
module: store
---
# A product catalog over HTTP

Products are identified by a three-letter code.

```wh
type Sku = str where matches(self, r"^[A-Z]{3}$")

struct Product:
    key sku: Sku
    name: str
    price: u32
    invariant self.price <= 100000
```

What can go wrong in the store, and in the API on top of it.

```wh
@error
enum StoreError:
    NotFound(str)
    Exists(str)

@error
enum ApiError:
    Store(from StoreError)
    BadPrice
```

The catalog, kept in memory.

```wh
capability Catalog:
    def put(own p: Product) -> unit raises StoreError
    def get(sku: Sku) -> Product raises StoreError
    def remove(sku: Sku) -> unit raises StoreError
    def all() -> list[Product]

adapter MemoryCatalog implements Catalog:
    pattern InMemory
```

Adding refuses free products; the rest delegates to the catalog.

```wh
def add_product(own p: Product) -> Product raises ApiError uses Catalog:
    raises ApiError.BadPrice when p.price == 0

def find_product(sku: Sku) -> Product raises StoreError uses Catalog

def delete_product(sku: Sku) -> unit raises StoreError uses Catalog

def search(prefix: str, limit: u8) -> list[Product] uses Catalog:
    ensures len(result) <= limit

flow create(p: Product) -> Product raises ApiError uses Catalog:
    p |> add_product
```

The HTTP API.

```wh
app store_api:
    target server(axum)
    log level="info" format="compact"
    listen "127.0.0.1:8080"
    provide Catalog = MemoryCatalog()
    route POST "/products" -> create
    route GET "/products/{sku}" -> find_product
    route DELETE "/products/{sku}" -> delete_product
    route GET "/search" -> search
    status StoreError.NotFound = 404
    status StoreError = 409
    status ApiError.BadPrice = 422
```
