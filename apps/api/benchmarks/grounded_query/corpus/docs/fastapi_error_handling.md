# Handling Errors - FastAPI Documentation

## Overview

There are many situations where you need to notify a client using your API about errors. You might need to tell them:

- The client doesn't have enough privileges for that operation
- The client doesn't have access to that resource
- The item the client was trying to access doesn't exist

In these cases, you return an **HTTP status code** in the range of **400** (400-499), which indicates client errors (similar to how 200-299 indicates success).

## Use `HTTPException`

### Import `HTTPException`

```python
from fastapi import FastAPI, HTTPException

app = FastAPI()
items = {"foo": "The Foo Wrestlers"}

@app.get("/items/{item_id}")
async def read_item(item_id: str):
    if item_id not in items:
        raise HTTPException(status_code=404, detail="Item not found")
    return {"item": items[item_id]}
```

### Raise an `HTTPException` in your code

`HTTPException` is a Python exception with additional data for APIs. You **raise** it (don't return it).

When raised inside a path operation function or utility function it calls, the exception terminates the request immediately and sends the HTTP error to the client without running remaining code.

Example with 404 response:

```python
from fastapi import FastAPI, HTTPException

app = FastAPI()
items = {"foo": "The Foo Wrestlers"}

@app.get("/items/{item_id}")
async def read_item(item_id: str):
    if item_id not in items:
        raise HTTPException(status_code=404, detail="Item not found")
    return {"item": items[item_id]}
```

### The resulting response

**Valid request** (`http://example.com/items/foo`):
```json
{
  "item": "The Foo Wrestlers"
}
```
HTTP Status: 200

**Invalid request** (`http://example.com/items/bar`):
```json
{
  "detail": "Item not found"
}
```
HTTP Status: 404

**Tip:** You can pass any JSON-convertible value as the `detail` parameter -- not just strings. You can pass dicts, lists, etc., and they'll be automatically converted to JSON by FastAPI.

## Add custom headers

You can add custom headers to HTTP errors for advanced scenarios:

```python
from fastapi import FastAPI, HTTPException

app = FastAPI()
items = {"foo": "The Foo Wrestlers"}

@app.get("/items-header/{item_id}")
async def read_item_header(item_id: str):
    if item_id not in items:
        raise HTTPException(
            status_code=404,
            detail="Item not found",
            headers={"X-Error": "There goes my error"},
        )
    return {"item": items[item_id]}
```

## Install custom exception handlers

You can add custom exception handlers using Starlette's exception utilities:

```python
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

class UnicornException(Exception):
    def __init__(self, name: str):
        self.name = name

app = FastAPI()

@app.exception_handler(UnicornException)
async def unicorn_exception_handler(request: Request, exc: UnicornException):
    return JSONResponse(
        status_code=418,
        content={"message": f"Oops! {exc.name} did something. There goes a rainbow..."},
    )

@app.get("/unicorns/{name}")
async def read_unicorn(name: str):
    if name == "yolo":
        raise UnicornException(name=name)
    return {"unicorn_name": name}
```

When requesting `/unicorns/yolo`, you'll receive:
```json
{"message": "Oops! yolo did something. There goes a rainbow..."}
```
HTTP Status: 418

## Override the default exception handlers

### Override request validation exceptions

When a request contains invalid data, FastAPI raises `RequestValidationError` internally. Override it with:

```python
from fastapi import FastAPI, HTTPException
from fastapi.exceptions import RequestValidationError
from fastapi.responses import PlainTextResponse
from starlette.exceptions import HTTPException as StarletteHTTPException

app = FastAPI()

@app.exception_handler(StarletteHTTPException)
async def http_exception_handler(request, exc):
    return PlainTextResponse(str(exc.detail), status_code=exc.status_code)

@app.exception_handler(RequestValidationError)
async def validation_exception_handler(request, exc: RequestValidationError):
    message = "Validation errors:"
    for error in exc.errors():
        message += f"\nField: {error['loc']}, Error: {error['msg']}"
    return PlainTextResponse(message, status_code=400)

@app.get("/items/{item_id}")
async def read_item(item_id: int):
    if item_id == 3:
        raise HTTPException(status_code=418, detail="Nope! I don't like 3.")
    return {"item_id": item_id}
```

Instead of the default JSON error format, invalid requests like `/items/foo` will return plain text:
```
Validation errors:
Field: ('path', 'item_id'), Error: Input should be a valid integer, unable to parse string as an integer
```

### Use the `RequestValidationError` body

The `RequestValidationError` contains the invalid body received, useful for debugging:

```python
from fastapi import FastAPI, Request
from fastapi.encoders import jsonable_encoder
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel

app = FastAPI()

@app.exception_handler(RequestValidationError)
async def validation_exception_handler(request: Request, exc: RequestValidationError):
    return JSONResponse(
        status_code=422,
        content=jsonable_encoder({"detail": exc.errors(), "body": exc.body}),
    )

class Item(BaseModel):
    title: str
    size: int

@app.post("/items/")
async def create_item(item: Item):
    return item
```

Sending invalid data like `{"title": "towel", "size": "XL"}` returns:
```json
{
  "detail": [
    {
      "loc": ["body", "size"],
      "msg": "value is not a valid integer",
      "type": "type_error.integer"
    }
  ],
  "body": {
    "title": "towel",
    "size": "XL"
  }
}
```

### FastAPI's `HTTPException` vs Starlette's `HTTPException`

FastAPI's `HTTPException` inherits from Starlette's `HTTPException`. The only difference is:
- **FastAPI**: accepts any JSON-able data for `detail`
- **Starlette**: only accepts strings for `detail`

When registering exception handlers, import and use Starlette's version to handle both:

```python
from starlette.exceptions import HTTPException as StarletteHTTPException
```

## Reuse FastAPI's exception handlers

You can import and reuse FastAPI's default exception handlers:

```python
from fastapi import FastAPI, HTTPException
from fastapi.exception_handlers import (
    http_exception_handler,
    request_validation_exception_handler,
)
from fastapi.exceptions import RequestValidationError
from starlette.exceptions import HTTPException as StarletteHTTPException

app = FastAPI()

@app.exception_handler(StarletteHTTPException)
async def custom_http_exception_handler(request, exc):
    print(f"OMG! An HTTP error!: {repr(exc)}")
    return await http_exception_handler(request, exc)

@app.exception_handler(RequestValidationError)
async def validation_exception_handler(request, exc):
    print(f"OMG! The client sent invalid data!: {exc}")
    return await request_validation_exception_handler(request, exc)

@app.get("/items/{item_id}")
async def read_item(item_id: int):
    if item_id == 3:
        raise HTTPException(status_code=418, detail="Nope! I don't like 3.")
    return {"item_id": item_id}
```

This approach allows you to add custom logging or processing while reusing FastAPI's default behavior.
