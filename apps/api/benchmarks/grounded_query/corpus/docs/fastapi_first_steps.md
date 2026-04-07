# First Steps - FastAPI

The simplest FastAPI file could look like this:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
async def root():
    return {"message": "Hello World"}
```

Copy that to a file `main.py`.

Run the live server:

```bash
$ fastapi dev
```

The output will show:
```
INFO:     Uvicorn running on http://127.0.0.1:8000 (Press CTRL+C to quit)
```

## Check it

Open your browser at `http://127.0.0.1:8000`.

You will see the JSON response as:
```json
{"message": "Hello World"}
```

## Interactive API docs

Now go to `http://127.0.0.1:8000/docs`.

You will see the automatic interactive API documentation provided by Swagger UI.

## Alternative API docs

Go to `http://127.0.0.1:8000/redoc`.

You will see the alternative automatic documentation provided by ReDoc.

## OpenAPI

**FastAPI** generates a "schema" with all your API using the **OpenAPI** standard for defining APIs.

### "Schema"

A "schema" is a definition or description of something. Not the code that implements it, but just an abstract description.

### API "schema"

OpenAPI is a specification that dictates how to define a schema of your API. This schema definition includes your API paths, the possible parameters they take, etc.

### Data "schema"

The term "schema" might also refer to the shape of some data, like a JSON content. It would mean the JSON attributes and data types they have.

### OpenAPI and JSON Schema

OpenAPI defines an API schema for your API. And that schema includes definitions (or "schemas") of the data sent and received by your API using **JSON Schema**, the standard for JSON data schemas.

### Check the `openapi.json`

If you are curious about how the raw OpenAPI schema looks like, FastAPI automatically generates a JSON (schema) with the descriptions of all your API.

You can see it directly at: `http://127.0.0.1:8000/openapi.json`.

It will show a JSON starting with something like:

```json
{
    "openapi": "3.1.0",
    "info": {
        "title": "FastAPI",
        "version": "0.1.0"
    },
    "paths": {
        "/items/": {
            "get": {
                "responses": {
                    "200": {
                        "description": "Successful Response",
                        "content": {
                            "application/json": { ...
```

### What is OpenAPI for

The OpenAPI schema is what powers the two interactive documentation systems included. There are dozens of alternatives, all based on OpenAPI. You could easily add any of those alternatives to your application built with **FastAPI**.

You could also use it to generate code automatically, for clients that communicate with your API (frontend, mobile, or IoT applications).

## Configure the app `entrypoint` in `pyproject.toml`

You can configure where your app is located in a `pyproject.toml` file like:

```toml
[tool.fastapi]
entrypoint = "main:app"
```

That `entrypoint` will tell the `fastapi` command that it should import the app like:

```python
from main import app
```

If your code was structured like:
```
.
├── backend
│   ├── main.py
│   ├── __init__.py
```

Then you would set the `entrypoint` as:

```toml
[tool.fastapi]
entrypoint = "backend.main:app"
```

Which would be equivalent to:

```python
from backend.main import app
```

## `fastapi dev` with path

You can also pass the file path to the `fastapi dev` command:

```bash
$ fastapi dev main.py
```

However, it is recommended to use the `entrypoint` in `pyproject.toml`.

## Recap, step by step

### Step 1: import `FastAPI`

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
async def root():
    return {"message": "Hello World"}
```

`FastAPI` is a Python class that provides all the functionality for your API. It inherits directly from `Starlette`, so you can use all Starlette functionality with FastAPI too.

### Step 2: create a `FastAPI` "instance"

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
async def root():
    return {"message": "Hello World"}
```

The `app` variable will be an "instance" of the class `FastAPI`. This will be the main point of interaction to create all your API.

### Step 3: create a _path operation_

#### Path

"Path" here refers to the last part of the URL starting from the first `/`.

So, in a URL like: `https://example.com/items/foo`, the path would be: `/items/foo`

A "path" is also commonly called an "endpoint" or a "route". While building an API, the "path" is the main way to separate "concerns" and "resources".

#### Operation

"Operation" here refers to one of the HTTP "methods":

- `POST`
- `GET`
- `PUT`
- `DELETE`
- `OPTIONS`
- `HEAD`
- `PATCH`
- `TRACE`

In the HTTP protocol, you can communicate to each path using one or more of these "methods".

Normally you use:

- `POST`: to create data.
- `GET`: to read data.
- `PUT`: to update data.
- `DELETE`: to delete data.

In OpenAPI, each of the HTTP methods is called an "operation".

#### Define a _path operation decorator_

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
async def root():
    return {"message": "Hello World"}
```

The `@app.get("/")` tells **FastAPI** that the function right below is in charge of handling requests that go to:

- the path `/`
- using a `get` operation

The `@something` syntax in Python is called a "decorator". A "decorator" takes the function below and does something with it. In this case, this decorator tells **FastAPI** that the function below corresponds to the **path** `/` with an **operation** `get`. It is the "**path operation decorator**".

You can also use the other operations:

- `@app.post()`
- `@app.put()`
- `@app.delete()`
- `@app.options()`
- `@app.head()`
- `@app.patch()`
- `@app.trace()`

You are free to use each operation (HTTP method) as you wish. **FastAPI** doesn't enforce any specific meaning.

### Step 4: define the **path operation function**

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
async def root():
    return {"message": "Hello World"}
```

This is our "**path operation function**":

- **path**: is `/`.
- **operation**: is `get`.
- **function**: is the function below the "decorator" (below `@app.get("/")`).

This is a Python function. It will be called by **FastAPI** whenever it receives a request to the URL "`/`" using a `GET` operation. In this case, it is an `async` function.

You could also define it as a normal function instead of `async def`:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
def root():
    return {"message": "Hello World"}
```

### Step 5: return the content

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
async def root():
    return {"message": "Hello World"}
```

You can return a `dict`, `list`, singular values as `str`, `int`, etc.

You can also return Pydantic models. There are many other objects and models that will be automatically converted to JSON (including ORMs, etc).

## Recap

- Import `FastAPI`.
- Create an `app` instance.
- Write a **path operation decorator** using decorators like `@app.get("/")`.
- Define a **path operation function**; for example, `def root(): ...`.
- Run the development server using the command `fastapi dev`.
