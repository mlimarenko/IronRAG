# GraphQL

## Overview

GraphQL is a declarative data query and manipulation language that enables clients to specify their exact data requirements. A GraphQL server can integrate data from multiple sources and return results in a unified graph structure. The language remains independent of any specific database or storage engine, with several open-source runtime engines available for implementation.

## History

Facebook initiated GraphQL development in 2012 and publicly released a draft specification along with a reference implementation as open source in 2015. By 2018, the GraphQL Foundation--hosted by the Linux Foundation--assumed stewardship of the project.

The GraphQL Schema Definition Language became an official part of the specification on February 9, 2018. Major technology companies including Facebook, GitHub, Yelp, Shopify, and Google have adopted GraphQL as their primary API access method. An annual GraphQL Conference showcases protocol advancements and organizational implementations, supported by the GraphQL Foundation and previously organized by Prisma and Hygraph.

## Design

GraphQL supports three core operations: reading data through queries, modifying data through mutations, and subscribing to real-time changes through subscriptions (typically implemented via WebSockets). Services are constructed by defining types with associated fields and providing resolver functions that retrieve and map the underlying data. After validation against the schema, the server executes queries and returns results mirroring the query structure, typically formatted as JSON.

### Type System

A GraphQL business domain is modeled as a graph by establishing a schema that defines various node types and their relationships. The type system describes queryable data, with the root Query type containing all available fields. Scalar base types represent fundamental values like strings, numbers, and identifiers.

Fields default to nullable status; appending an exclamation mark designates a field as required. List types are denoted by wrapping a field's type in square brackets (e.g., `authors: [String]`).

### Queries

Queries define the precise data structure a client requires. The server returns data matching the requested shape:

```graphql
query CurrentUser {
  currentUser {
    name
    age
  }
}
```

Response:
```json
{
  "currentUser": {
    "name": "John Doe",
    "age": 23
  }
}
```

### Mutations

Mutations enable data creation, updates, and deletion. They typically include variables for passing client data to the server and specify the response structure:

```graphql
mutation CreateUser($name: String!, $age: Int!) {
  createUser(userName: $name, age: $age) {
    name
    age
  }
}
```

Variables object:
```json
{
  "name": "Han Solo",
  "age": 42
}
```

Server response:
```json
{
  "data": {
    "createUser": {
      "name": "Han Solo",
      "age": 42
    }
  }
}
```

### Subscriptions

Subscriptions enable live server-to-client data delivery when mutations occur. The client defines required data shape for updates:

```graphql
subscription {
  newPerson {
    name
    age
  }
}
```

Subsequent mutations trigger formatted data delivery to subscribed clients.

### Versioning

GraphQL emphasizes continuous schema evolution over traditional versioning. The `@deprecated` directive marks obsolete schema elements, signaling clients without breaking functionality. Since GraphQL returns only explicitly requested data, new types and fields can be added without creating breaking changes, enabling a versionless API approach.

### Comparison to Other Query Languages

GraphQL differs fundamentally from comprehensive graph query languages like SPARQL or SQL hierarchical extensions. It lacks support for transitive closure operations--for instance, a single GraphQL query cannot retrieve all ancestors of an individual when the schema only provides parent relationships.

## Testing

GraphQL APIs support both manual and automated testing through tools that issue GraphQL requests and verify response correctness. Automatic test generation is feasible through search-based techniques leveraging the typed schema and introspection capabilities.

Testing tools include Postman, Beeceptor, GraphiQL, Apollo Studio, GraphQL Hive, GraphQL Editor, and Step CI.
