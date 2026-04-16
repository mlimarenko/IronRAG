# Rewards Accounts API Contract

The rewards accounts surface is a REST API that returns JSON over HTTP.

## Canonical Endpoint

- Method: `GET`
- Path: `/v1/accounts`
- Transport contract: REST over HTTP with JSON payloads.

## Pagination And Expansion Parameters

| Parameter | Meaning |
| --- | --- |
| `pageNumber` | 1-based page number |
| `pageSize` | number of accounts per page |
| `withCards` | include linked card records in the response |
| `numberStarting` | prefix filter for the account number |

## Transport Comparison

Compared with the inventory SOAP surface, rewards accounts use REST JSON over HTTP instead of SOAP described by WSDL.

## Unsupported Transports

The rewards accounts API does not publish a GraphQL API.
