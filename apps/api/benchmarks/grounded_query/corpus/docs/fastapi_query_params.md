# Query Parameters - FastAPI Documentation

When you declare other function parameters that are not part of the path parameters, they are automatically interpreted as "query" parameters.

## Overview

The query is the set of key-value pairs that go after the `?` in a URL, separated by `&` characters.

For example, in the URL:
```
http://127.0.0.1:8000/items/?skip=0&limit=10
```

The query parameters are:
- `skip`: with a value of `0`
- `limit`: with a value of `10`

As they are part of the URL, they are "naturally" strings. However, when you declare them with Python types, they are converted to that type and validated against it.

All the same process that applied for path parameters also applies for query parameters:
- Editor support
- Data "parsing"
- Data validation
- Automatic documentation

## Basic Example

```python
from fastapi import FastAPI

app = FastAPI()

fake_items_db = [{"item_name": "Foo"}, {"item_name": "Bar"}, {"item_name": "Baz"}]

@app.get("/items/")
async def read_item(skip: int = 0, limit: int = 10):
    return fake_items_db[skip : skip + limit]
```

## Defaults

Query parameters are not a fixed part of a path, so they can be optional and can have default values.

In the example above, `skip=0` and `limit=10` are defaults.

Going to `http://127.0.0.1:8000/items/` is the same as going to `http://127.0.0.1:8000/items/?skip=0&limit=10`

If you go to `http://127.0.0.1:8000/items/?skip=20`, the parameter values will be:
- `skip=20`: because you set it in the URL
- `limit=10`: because that was the default value

## Optional Parameters

You can declare optional query parameters by setting their default to `None`:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def read_item(item_id: str, q: str | None = None):
    if q:
        return {"item_id": item_id, "q": q}
    return {"item_id": item_id}
```

**FastAPI** is smart enough to notice that `item_id` is a path parameter and `q` is a query parameter.

## Query Parameter Type Conversion

You can declare `bool` types, and they will be converted:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def read_item(item_id: str, q: str | None = None, short: bool = False):
    item = {"item_id": item_id}
    if q:
        item.update({"q": q})
    if not short:
        item.update(
            {"description": "This is an amazing item that has a long description"}
        )
    return item
```

If you go to any of these URLs:
- `http://127.0.0.1:8000/items/foo?short=1`
- `http://127.0.0.1:8000/items/foo?short=True`
- `http://127.0.0.1:8000/items/foo?short=true`
- `http://127.0.0.1:8000/items/foo?short=on`
- `http://127.0.0.1:8000/items/foo?short=yes`

Your function will see the parameter `short` with a `bool` value of `True`. Otherwise it will be `False`.

## Multiple Path and Query Parameters

You can declare multiple path parameters and query parameters at the same time. **FastAPI** knows which is which by name, and you don't have to declare them in any specific order:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/users/{user_id}/items/{item_id}")
async def read_user_item(
    user_id: int, item_id: str, q: str | None = None, short: bool = False
):
    item = {"item_id": item_id, "owner_id": user_id}
    if q:
        item.update({"q": q})
    if not short:
        item.update(
            {"description": "This is an amazing item that has a long description"}
        )
    return item
```

## Required Query Parameters

When you declare a default value for non-path parameters, it is not required. However, when you want to make a query parameter required, simply don't declare any default value:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def read_user_item(item_id: str, needy: str):
    item = {"item_id": item_id, "needy": needy}
    return item
```

Here, `needy` is a required query parameter of type `str`.

If you access `http://127.0.0.1:8000/items/foo-item` without the required `needy` parameter, you will get an error:

```json
{
  "detail": [
    {
      "type": "missing",
      "loc": [
        "query",
        "needy"
      ],
      "msg": "Field required",
      "input": null
    }
  ]
}
```

You need to set it in the URL: `http://127.0.0.1:8000/items/foo-item?needy=sooooneedy`

## Mixed Parameters

You can define some parameters as required, some with default values, and some entirely optional:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def read_user_item(
    item_id: str, needy: str, skip: int = 0, limit: int | None = None
):
    item = {"item_id": item_id, "needy": needy, "skip": skip, "limit": limit}
    return item
```

In this case, there are 3 query parameters:
- `needy`: a required `str`
- `skip`: an `int` with a default value of `0`
- `limit`: an optional `int`
