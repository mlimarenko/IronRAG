# Request Body - FastAPI Documentation

## Overview

When you need to send data from a client (like a browser) to your API, you send it as a **request body**.

- A **request body** is data sent by the client to your API
- A **response body** is the data your API sends to the client

Your API almost always has to send a response body, but clients don't necessarily need to send request bodies all the time.

To declare a request body, use Pydantic models with all their power and benefits.

> **Info**: To send data, use one of: `POST` (most common), `PUT`, `DELETE`, or `PATCH`. Sending a body with a `GET` request has undefined behavior in specifications and is discouraged.

## Import Pydantic's `BaseModel`

```python
from fastapi import FastAPI
from pydantic import BaseModel

class Item(BaseModel):
    name: str
    description: str | None = None
    price: float
    tax: float | None = None

app = FastAPI()

@app.post("/items/")
async def create_item(item: Item):
    return item
```

## Create Your Data Model

Declare your data model as a class that inherits from `BaseModel` using standard Python types for all attributes.

When a model attribute has a default value, it is not required. Otherwise, it is required. Use `None` to make it optional.

The model above declares a JSON object like:

```json
{
    "name": "Foo",
    "description": "An optional description",
    "price": 45.2,
    "tax": 3.5
}
```

This would also be valid (since `description` and `tax` are optional):

```json
{
    "name": "Foo",
    "price": 45.2
}
```

## Declare it as a Parameter

Add the model to your path operation by declaring it the same way you declare path and query parameters:

```python
@app.post("/items/")
async def create_item(item: Item):
    return item
```

Declare its type as the model you created, `Item`.

## Results

With just that Python type declaration, **FastAPI** will:

- Read the body of the request as JSON
- Convert the corresponding types (if needed)
- Validate the data (returning nice error messages if invalid)
- Give you the received data in the parameter `item`
- Provide editor support for all attributes and their types
- Generate JSON Schema definitions for your model
- Include those schemas in the generated OpenAPI schema and automatic documentation UIs

## Automatic Docs

The JSON Schemas of your models will be part of your OpenAPI generated schema and shown in the interactive API docs.

## Editor Support

In your editor, inside your function you get type hints and completion everywhere. You also get error checks for incorrect type operations. This works with Visual Studio Code, PyCharm, and most other Python editors.

## Use the Model

Inside the function, access all attributes of the model object directly:

```python
@app.post("/items/")
async def create_item(item: Item):
    item_dict = item.model_dump()
    if item.tax is not None:
        price_with_tax = item.price + item.tax
        item_dict.update({"price_with_tax": price_with_tax})
    return item_dict
```

## Request Body + Path Parameters

You can declare path parameters and request body at the same time. **FastAPI** recognizes that function parameters matching path parameters should be taken from the path, and Pydantic models should be taken from the request body.

```python
@app.put("/items/{item_id}")
async def update_item(item_id: int, item: Item):
    return {"item_id": item_id, **item.model_dump()}
```

## Request Body + Path + Query Parameters

You can declare **body**, **path**, and **query** parameters all at the same time.

```python
@app.put("/items/{item_id}")
async def update_item(item_id: int, item: Item, q: str | None = None):
    result = {"item_id": item_id, **item.model_dump()}
    if q:
        result.update({"q": q})
    return result
```

Function parameters are recognized as follows:

- If declared in the **path**, it will be a path parameter
- If it's a **singular type** (`int`, `float`, `str`, `bool`, etc), it's a **query** parameter
- If declared to be a **Pydantic model**, it's a request **body**

> **Note**: FastAPI knows `q` is not required because of the default value `= None`. The `str | None` type annotation helps editors provide better support.

## Without Pydantic

If you don't want to use Pydantic models, you can use **Body** parameters. See the docs for Body - Multiple Parameters: Singular values in body.
