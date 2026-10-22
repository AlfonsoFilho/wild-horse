---
module: signup
---
# Signup

People sign up with an email address and their age. Minors may only sign up
when a guardian who already has an account vouches for them. The whole
process is the [[register]] flow at the end of this document.

## Domain

An email address as typed by the user. We only check its shape here;
whether it can receive mail is somebody else's problem. An address is
personal data, so it is `@sensitive`: it shows up as `<redacted>` in every
log, trace and error message, wherever it travels.

```wh
@sensitive
type Email = str where matches(self, r"^[^@\s]+@[^@\s]+$")
type Age = u8 where self <= 150
alias UserId = u64
```

A registered user. Users under 13 must have a guardian.

```wh
struct User:
    key id: UserId
    sensitive email: Email
    age: Age
    guardian: UserId | None = None
    invariant self.age >= 13 or self.guardian is not None
```

The raw form exactly as submitted, and the same form once its fields have
been checked.

```wh
struct SignupForm:
    sensitive email: str
    age: int
    guardian: UserId | None = None

struct ValidForm:
    sensitive email: Email
    age: Age
    guardian: UserId | None = None
```

## Errors

```wh
@error
enum ValidationError:
    BadEmail(str)
    BadAge(int)
    DisposableEmail(str)

@error
enum DbError:
    Duplicate(Email)
    Unavailable

@error
enum RegisterError:
    Invalid(from ValidationError)
    TooYoung
    Storage(from DbError)
```

## Validation and normalization

Checking a form turns loosely typed input into domain values, or explains
what is wrong with it.

Addresses at the disposable domain `example.org` are refused outright: nobody
can be reached there, so an account created with one could never be
confirmed. The domain is matched case-insensitively, as emails are everywhere
else, and the form is rejected with its own `DisposableEmail` variant of
[[ValidationError]] so callers can tell it apart from a malformed address.

```wh
def validate(form: SignupForm) -> ValidForm raises ValidationError:
    raises ValidationError when not (0 <= form.age <= 150)
    raises ValidationError.DisposableEmail when form.email.lower().endswith("@example.org")
    example validate(SignupForm(email="a@b.io", age=30)).age == 30
    example validate(SignupForm(email="nope", age=30)) raises ValidationError
    example validate(SignupForm(email="a@b.io", age=-1)) raises ValidationError
    example validate(SignupForm(email="Someone@Example.org", age=30)) raises ValidationError.DisposableEmail
```

Emails are compared case-insensitively, so we store a canonical form.

```wh
def normalize_email(e: Email) -> Email:
    """Canonical form used for uniqueness checks."""
    ensures result == e.lower()
    example normalize_email("A@B.com") == "a@b.com"
    property idempotent(x: Email):
        normalize_email(normalize_email(x)) == normalize_email(x)
    pattern Transform

def normalize_form(own form: ValidForm) -> ValidForm:
    ensures result.email == normalize_email(form.email)
    ensures result.age == form.age
    pattern Transform
```

Minors need a guardian.

```wh
def check_age(own form: ValidForm) -> ValidForm raises RegisterError:
    raises RegisterError.TooYoung when form.age < 13 and form.guardian is None
    example check_age(ValidForm(email="k@x.io", age=12)) raises RegisterError
    example check_age(ValidForm(email="a@x.io", age=40)).age == 40
    example check_age(ValidForm(email="t@x.io", age=13)).age == 13
```

Bulk imports arrive with duplicates. Deduplication must keep the first
occurrence of each address and must stay linear, because imports can be large.

```wh
def dedupe_emails(emails: list[Email]) -> list[Email]:
    ensures len(result) <= len(emails)
    ensures all(e in emails for e in result)
    ensures len(set(result)) == len(result)
    example dedupe_emails(["a@b.io", "c@d.io", "a@b.io"]) == ["a@b.io", "c@d.io"]
    complexity O(n)
    optimize speed
```

## Storage

```wh
capability Db:
    def insert(own form: ValidForm) -> UserId raises DbError
    def find_by_email(email: Email) -> User | None raises DbError
```

An in-memory database, used by the CLI and by tests.

```wh
adapter MemoryDb implements Db:
    pattern InMemory
    note "store users in a Mutex<Vec<User>>; ids start at 1 and increase by one"
    note "insert fails with DbError.Duplicate if the email is already taken"
```

```wh
def save_user(own form: ValidForm) -> UserId raises DbError uses Db:
    note "delegate to Db.insert"
```

## Registering

The registration flow: validate, normalize, check the age rule, store.

```wh
flow register(form: SignupForm) -> UserId raises RegisterError uses Db:
    form
    |> validate
    |> normalize_form
    |> check_age
    |> save_user
```

```wh
test "minors need a guardian":
    given Db = MemoryDb()
    expect register(SignupForm(email="kid@x.io", age=12)) raises RegisterError
    expect register(SignupForm(email="kid@x.io", age=12, guardian=7)) == 1

test "emails are unique ignoring case":
    given Db = MemoryDb()
    expect register(SignupForm(email="a@x.io", age=30)) == 1
    expect register(SignupForm(email="A@X.io", age=31)) raises RegisterError
```
