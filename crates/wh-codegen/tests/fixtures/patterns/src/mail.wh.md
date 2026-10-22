---
module: mail
---
# Async mail

Sending mail is asynchronous; an address without `@` is rejected.

```wh
@error
enum MailError:
    Rejected(str)

capability Mailer:
    async def send(to: str, body: str) -> unit raises MailError:
        raises MailError when not to.contains("@")
    def sent() -> list[str]:
        ensures all(m.contains("@") for m in result)

adapter MemoryMailer implements Mailer:
    pattern InMemory
```

Welcoming somebody sends them a mail and says to whom.

```wh
async def notify(to: str, name: str) -> str raises MailError uses Mailer:
    note "send `hello <name>` to `to`, then return `sent to <to>`"

def outbox() -> list[str] uses Mailer:
    note "delegate to Mailer.sent"

flow welcome(to: str, name: str) -> str raises MailError uses Mailer:
    notify(to, name)

flow welcome_both(a: str, b: str) -> tuple[str, str] raises MailError uses Mailer:
    x, y = parallel(welcome(a, "A"), welcome(b, "B"))
    return (x, y)
```

```wh
test "welcome mails":
    given Mailer = MemoryMailer()
    expect welcome("a@x.io", "Ann") == "sent to a@x.io"
    expect welcome_both("b@x.io", "c@x.io") == ("sent to b@x.io", "sent to c@x.io")
    expect outbox() == ["a@x.io", "b@x.io", "c@x.io"]
    expect welcome("nope", "X") raises MailError
```

The command line welcomes the address it is given.

```wh
def first_arg(args: list[str]) -> str:
    note "the first argument after the program name, or an empty string"

app mailer:
    target native(cli)
    log level="off"
    provide Mailer = MemoryMailer()
    entry main(args: list[str]) -> unit raises MailError:
        args |> first_arg |> welcome_one

flow welcome_one(to: str) -> str raises MailError uses Mailer:
    welcome(to, "you")
```
