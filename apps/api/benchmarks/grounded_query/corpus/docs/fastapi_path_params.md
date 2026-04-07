# Path Parameters - FastAPI Documentation

## Path Parameters

You can declare path "parameters" or "variables" with the same syntax used by Python format strings:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def read_item(item_id):
    return {"item_id": item_id}
```

The value of the path parameter `item_id` will be passed to your function as the argument `item_id`.

If you run this example and go to `http://127.0.0.1:8000/items/foo`, you will see a response of:

```json
{"item_id":"foo"}
```

## Path parameters with types

You can declare the type of a path parameter in the function, using standard Python type annotations:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/items/{item_id}")
async def read_item(item_id: int):
    return {"item_id": item_id}
```

In this case, `item_id` is declared to be an `int`. This will give you editor support inside of your function, with error checks, completion, etc.

## Data conversion

If you run this example and open your browser at `http://127.0.0.1:8000/items/3`, you will see a response of:

```json
{"item_id":3}
```

Notice that the value your function received (and returned) is `3`, as a Python `int`, not a string `"3"`. So, with that type declaration, **FastAPI** gives you automatic request "parsing".

## Data validation

If you go to the browser at `http://127.0.0.1:8000/items/foo`, you will see a nice HTTP error:

```json
{
   "detail": [
    {
      "type": "int_parsing",
      "loc": [
        "path",
        "item_id"
      ],
      "msg": "Input should be a valid integer, unable to parse string as an integer",
      "input": "foo"
    }
  ]
}
```

Because the path parameter `item_id` had a value of `"foo"`, which is not an `int`. So, with the same Python type declaration, **FastAPI** gives you data validation.

## Documentation

When you open your browser at `http://127.0.0.1:8000/docs`, you will see an automatic, interactive, API documentation. Again, just with that same Python type declaration, **FastAPI** gives you automatic, interactive documentation (integrating Swagger UI).

## Standards-based benefits, alternative documentation

Because the generated schema is from the OpenAPI standard, there are many compatible tools. **FastAPI** itself provides an alternative API documentation (using ReDoc), which you can access at `http://127.0.0.1:8000/redoc`.

## Pydantic

All the data validation is performed under the hood by Pydantic. You can use the same type declarations with `str`, `float`, `bool` and many other complex data types.

## Order matters

When creating _path operations_, you need to make sure that the path for fixed paths is declared before paths with parameters.

For example, for `/users/me` and `/users/{user_id}`, declare `/users/me` first:

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/users/me")
async def read_user_me():
    return {"user_id": "the current user"}

@app.get("/users/{user_id}")
async def read_user(user_id: str):
    return {"user_id": user_id}
```

Otherwise, the path for `/users/{user_id}` would match also for `/users/me`, "thinking" that it's receiving a parameter `user_id` with a value of `"me"`.

Similarly, you cannot redefine a path operation - the first one will always be used since the path matches first.

## Predefined values

If you want the possible valid _path parameter_ values to be predefined, you can use a standard Python `Enum`.

### Create an `Enum` class

Import `Enum` and create a sub-class that inherits from `str` and from `Enum`. By inheriting from `str` the API docs will be able to know that the values must be of type `string` and will be able to render correctly.

```python
from enum import Enum
from fastapi import FastAPI

class ModelName(str, Enum):
    alexnet = "alexnet"
    resnet = "resnet"
    lenet = "lenet"

app = FastAPI()

@app.get("/models/{model_name}")
async def get_model(model_name: ModelName):
    if model_name is ModelName.alexnet:
        return {"model_name": model_name, "message": "Deep Learning FTW!"}
    if model_name.value == "lenet":
        return {"model_name": model_name, "message": "LeCNN all the images"}
    return {"model_name": model_name, "message": "Have some residuals"}
```

### Working with Python _enumerations_

The value of the _path parameter_ will be an _enumeration member_.

#### Compare _enumeration members_

You can compare it with the _enumeration member_ in your created enum:

```python
if model_name is ModelName.alexnet:
    return {"model_name": model_name, "message": "Deep Learning FTW!"}
```

#### Get the _enumeration value_

You can get the actual value (a `str` in this case) using `model_name.value`:

```python
if model_name.value == "lenet":
    return {"model_name": model_name, "message": "LeCNN all the images"}
```

#### Return _enumeration members_

You can return _enum members_ from your _path operation_, even nested in a JSON body. They will be converted to their corresponding values (strings in this case) before returning them to the client:

```json
{
   "model_name": "alexnet",
  "message": "Deep Learning FTW!"
}
```

## Path parameters containing paths

Let's say you have a _path operation_ with a path `/files/{file_path}`. But you need `file_path` itself to contain a _path_, like `home/johndoe/myfile.txt`.

### Path convertor

Using an option directly from Starlette you can declare a _path parameter_ containing a _path_ using a URL like:

```
/files/{file_path:path}
```

In this case, the name of the parameter is `file_path`, and the last part, `:path`, tells it that the parameter should match any _path_.

```python
from fastapi import FastAPI

app = FastAPI()

@app.get("/files/{file_path:path}")
async def read_file(file_path: str):
    return {"file_path": file_path}
```

You might need the parameter to contain `/home/johndoe/myfile.txt`, with a leading slash (`/`). In that case, the URL would be: `/files//home/johndoe/myfile.txt`, with a double slash (`//`) between `files` and `home`.

## Recap

With **FastAPI**, by using short, intuitive and standard Python type declarations, you get:

- Editor support: error checks, autocompletion, etc.
- Data "parsing"
- Data validation
- API annotation and automatic documentation

And you only have to declare them once. That's probably the main visible advantage of **FastAPI** compared to alternative frameworks.
