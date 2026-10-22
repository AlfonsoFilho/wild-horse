---
module: server
---
# Signup over HTTP

The [[signup]] spec, unchanged, served as a JSON API instead of a command
line. The server adds no logic of its own: it maps a route to the
[[signup.register]] flow and the flow's errors to HTTP statuses.

The holes are the ones of the signup example, so this project builds against
its implementations:

```text
wh gen examples/signup_server --impl-root examples/signup/impl
```

```wh
from signup import register, Db, MemoryDb, RegisterError, ValidationError, DbError
```

`POST /signup` takes a signup form as JSON (`{"email": "a@b.io", "age": 30}`)
and answers with the new user's id.

- A malformed form is the client's fault (`400`).
- A minor without a guardian is a valid request that cannot be served (`422`).
- An address that is already registered is a conflict (`409`).
- A database that is down is `503`.

The users live in memory, so they are gone when the server stops.

```wh
app signup_api:
    target server(axum)
    log level="info" format="compact"
    listen "127.0.0.1:3000"
    provide Db = MemoryDb()
    route POST "/signup" -> register
    status ValidationError = 400
    status RegisterError.TooYoung = 422
    status DbError.Duplicate = 409
    status DbError.Unavailable = 503
```
