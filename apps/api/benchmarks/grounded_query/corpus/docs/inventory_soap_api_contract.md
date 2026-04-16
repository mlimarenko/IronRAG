# Inventory SOAP API Contract

The inventory integration surface is published as SOAP over HTTP and described by a canonical WSDL contract.

## Canonical WSDL

- WSDL URL: `http://demo.local:8080/inventory-api/ws/inventory.wsdl`
- Transport contract: SOAP messages over HTTP, described by WSDL.
- Contract style: RPC-style operations with XML envelopes.

Example operation families:

- `GetInventorySnapshot`
- `FindInventoryBySku`
- `ReserveInventory`

## Notes

- This surface is not REST.
- This surface is not GraphQL.
- Agents that need the service description must use the WSDL URL `http://demo.local:8080/inventory-api/ws/inventory.wsdl`.
