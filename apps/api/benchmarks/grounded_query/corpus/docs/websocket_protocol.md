# WebSocket

## Overview

WebSocket is a communications protocol enabling bidirectional communication over a single TCP connection. The IETF standardized it as RFC 6455 in 2011, with the WHATWG maintaining the current living standard specification known as _WebSockets_.

The protocol differs from HTTP, yet maintains compatibility through the HTTP Upgrade header during the handshake process. This design allows WebSocket to operate over standard HTTP ports (80 and 443) while supporting proxies and intermediaries.

## Key Characteristics

WebSocket enables full-duplex interaction between web browsers and servers with significantly lower overhead than alternatives like HTTP polling. The protocol allows:

- Servers to send content without client requests
- Messages to be exchanged while maintaining an open connection
- Real-time data transfer in both directions

Communications typically occur over TCP port 443 (or 80 for unsecured connections), beneficial for firewall-restricted environments.

## History

WebSocket originated as "TCPConnection" in the HTML5 specification. In June 2008, discussions led by Michael Carter produced the first protocol version. Ian Hickson and Michael Carter coined the term "WebSocket" through IRC collaboration.

In December 2009, Google Chrome 4 was the first browser to ship full support for the standard, with WebSocket enabled by default. Development moved from W3C and WHATWG to IETF in February 2010, with RFC 6455 finalized in December 2011 under Ian Fette.

RFC 7692 later introduced compression extensions using DEFLATE on a per-message basis.

## Web API

### Constructor and Methods

The WebSocket interface provides:

- **Constructor**: `new WebSocket(url [, protocols])` initiates the opening handshake
- **send(data)**: Transmits data messages (string, Blob, ArrayBuffer, or ArrayBufferView)
- **close([code] [, reason])**: Begins the closing handshake

### Events

- **onopen**: Fires when the opening handshake succeeds
- **onmessage**: Fires when data arrives (only in OPEN state)
- **onclose**: Fires when the TCP connection closes
- **onerror**: Fires on connection errors

### Attributes

- **readyState**: Indicates connection state (CONNECTING=0, OPEN=1, CLOSING=2, CLOSED=3)
- **binaryType**: Sets whether binary data arrives as Blob or ArrayBuffer
- **url**: The WebSocket URL with transformed scheme (http->ws, https->wss)
- **bufferedAmount**: Queued application data awaiting transmission
- **protocol**: Server-accepted protocol from negotiation
- **extensions**: Accepted protocol-level extensions

## Protocol Specification

### Opening Handshake

The client sends an HTTP GET request (version >=1.1) and the server responds with HTTP 101 status (Switching Protocols) on success. Key headers include:

| Header | Direction | Purpose |
|--------|-----------|---------|
| Sec-WebSocket-Key | Request | base64-encoded 16 random bytes |
| Sec-WebSocket-Accept | Response | SHA1 hash of key + magic string |
| Sec-WebSocket-Version | Request | Protocol version (13) |
| Sec-WebSocket-Protocol | Both | Application-level protocol negotiation |
| Sec-WebSocket-Extensions | Both | Protocol extension negotiation |

After receiving the 101 response, HTTP ceases and communication switches to binary frame-based protocol.

### Frame-Based Messages

Messages consist of one frame (unfragmented) or multiple frames (fragmented). Fragmentation enables sending messages when complete length is unknown, avoiding unnecessary buffering.

- **Unfragmented**: Single frame with FIN=1 and opcode!=0
- **Fragmented**: Opening frame (FIN=0, opcode!=0) followed by continuation frames (FIN=0, opcode=0) and terminating frame (FIN=1, opcode=0)

### Frame Structure

Each frame contains:

| Field | Size | Purpose |
|-------|------|---------|
| FIN | 1 bit | 1=final frame; 0=fragmented |
| RSV1-3 | 3 bits | Reserved; must be 0 unless defined by extension |
| Opcode | 4 bits | Frame type identifier |
| Masked | 1 bit | 1=frame is masked (client frames only) |
| Payload length | 7, 7+16, or 7+64 bits | Message size encoding |
| Masking key | 0 or 32 bits | Present if masked=1 |
| Payload | Variable | Extension and application data |

### Opcodes

| Opcode | Type | Purpose | Fragmentable |
|--------|------|---------|--------------|
| 0 | Continuation | Non-first frame of fragmented message | Yes |
| 1 | Text | UTF-8 encoded text data | Yes |
| 2 | Binary | Binary data | Yes |
| 8 | Close | Initiates closing handshake | No |
| 9 | Ping | Latency measurement and keepalive | No |
| 10 | Pong | Response to ping | No |

### Client-to-Server Masking

Clients must mask all frames; servers must not mask. Masking applies XOR between payload and a random 32-bit key: `payload[i] := payload[i] xor masking_key[i mod 4]`

This prevents proxy cache poisoning and intermediate manipulation.

### Status Codes

Defined ranges for close frame payloads:

| Range | Purpose |
|-------|---------|
| 1000-1011 | Protocol-defined codes (normal closure, protocol error, unsupported data, etc.) |
| 3000-3999 | Library/framework/application-reserved, registered with IANA |
| 4000-4999 | Private use |

Key codes include:
- **1000**: Normal closure
- **1001**: Going away (browser tab closed; server shutting down)
- **1002**: Protocol error
- **1006**: Connection closed abnormally (no handshake)
- **1009**: Message too big

### Compression Extension

The `permessage-deflate` extension enables DEFLATE-based data message compression. Clients and servers negotiate this via the Sec-WebSocket-Extensions header. The RSV1 field indicates compressed payload data.

## Browser Support

WebSocket implementation varies by browser and protocol version:

- **Firefox 6+**: Secure version
- **Safari 6+**: Secure version
- **Google Chrome 14+**: RFC 6455 (v13)
- **Microsoft Edge/IE 10+**: RFC 6455
- **Opera 12.10+**: RFC 6455

Earlier versions (Opera 11, Safari 5) implemented older, less secure protocols. Firefox 4-5 and Opera 11 disabled WebSocket due to vulnerabilities.

## Server Implementations

Major web servers supporting WebSocket include:

- **Nginx** (v1.3.13+): Full support with reverse proxy and load balancing
- **Apache HTTP Server** (v2.4.5+): WebSocket support since July 2013
- **Internet Information Services** (IIS 8+): Included with Windows Server 2012
- **lighttpd** (v1.4.46+): Full support; v1.4.65+ supports WebSocket over HTTP/2
- **Eclipse Mosquitto**: MQTT broker with WebSocket support

ASP.NET Core provides WebSocket support via middleware.

## Security Considerations

Unlike cross-domain HTTP requests, WebSocket connections are not restricted by same-origin policy. Servers must validate the Origin header against expected origins to prevent cross-site WebSocket hijacking attacks.

It is better to use tokens or similar protection mechanisms to authenticate the WebSocket connection when sensitive (private) data is being transferred over the WebSocket. Authentication via cookies or HTTP headers alone is insufficient for sensitive operations.

The 2020 Cable Haunt vulnerability demonstrated real-world exploitation of inadequate WebSocket security.

## Proxy Traversal

WebSocket clients attempt to detect proxy configuration and use HTTP CONNECT to establish persistent tunnels. However:

- Transparent proxies unaware of WebSocket often block connections
- Encrypted WebSocket Secure (WSS) connections ensure tunnel establishment through explicit proxies via TLS
- Intermediate transparent proxies may allow encrypted traffic through more readily than unencrypted

Earlier protocol drafts (hixie-76) broke compatibility with reverse proxies by including key data after headers without Content-Length advertising, leading to intermediary forwarding failures. Modern drafts resolved this by placing key data in headers.
