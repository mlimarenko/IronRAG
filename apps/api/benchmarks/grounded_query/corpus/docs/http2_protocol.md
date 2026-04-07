# HTTP/2: A Major Web Protocol Revision

## Overview

HTTP/2 is a substantial upgrade to the HTTP network protocol that powers the web. As the first major revision since HTTP/1.1 was standardized in 1997, HTTP/2 was officially published as RFC 7540 on May 14, 2015. The protocol emerged from Google's experimental SPDY project and was developed through the Internet Engineering Task Force's HTTP Working Group (httpbis).

## Key Objectives

The working group established several critical goals for HTTP/2:

- Create negotiation mechanisms allowing clients and servers to select HTTP versions
- Maintain backward compatibility with HTTP/1.1 semantics (methods, status codes, URIs, headers)
- Reduce latency to accelerate webpage loading through header compression, server push capabilities, request prioritization, and multiplexing over single TCP connections
- Support existing HTTP use cases across browsers, mobile devices, web APIs, and various server architectures

## Major Improvements Over HTTP/1.1

While HTTP/2 preserves the high-level semantics of its predecessor, the protocol introduces fundamental changes in how data is framed and transmitted. The specification states that "new applications can take advantage of new features for increased speed."

### Performance Enhancements

The protocol addresses a critical HTTP/1.1 limitation through multiplexing--handling multiple concurrent requests across a single TCP connection. This eliminates the head-of-line blocking problems that plagued earlier versions, even when HTTP pipelining was employed. Additionally, HTTP/2 implements fixed Huffman code-based header compression, replacing SPDY's stream-based approach to mitigate compression oracle attacks like CRIME.

Server Push represents another significant feature, allowing servers to proactively transmit resources a browser will need before receiving explicit requests, eliminating latency from additional request cycles.

## Historical Development

SPDY, developed by Google as an experimental protocol, served as HTTP/2's foundation. SPDY introduced stream IDs enabling single TCP channels with separate control and data frames. Testing demonstrated "page load speedup ranging from 11% to 47%" compared to HTTP/1.1.

The standardization timeline began in December 2007 with HTTP/1.1 revision drafts. The IETF accepted HTTP/2 as a Proposed Standard in February 2015, following a December 2014 submission.

## Encryption Considerations

HTTP/2 operates in two configurations: h2c (unencrypted HTTP URIs) and h2 (HTTPS using TLS 1.2+ with ALPN extension). Although the standard permits unencrypted deployment, all major browser implementations--Chrome, Edge, Firefox, Internet Explorer, Opera, Safari--enforce encryption as a practical requirement.

## Notable Criticisms

### Protocol Complexity and Development

Poul-Henning Kamp, a respected BSD and Varnish developer, argues the accelerated development timeline restricted alternatives to SPDY, resulting in "inconsistent and overwhelmingly complex" specifications that violate protocol layering principles by duplicating TCP-level flow control.

### Head-of-Line Blocking Limitations

While HTTP/2 resolves application-layer blocking, packet-level TCP congestion still affects all multiplexed streams simultaneously. This architectural limitation drove significant development effort behind QUIC and HTTP/3.

### Encryption Overhead Debate

Disagreement persisted regarding mandatory encryption. Critics noted computational costs and argued many applications lack encryption requirements. Proponents countered that overhead remains negligible in practice. The working group ultimately failed to achieve consensus on mandatory encryption, though de facto industry requirements emerged through browser implementations.

## Browser and Server Adoption

By late 2015, major browsers had added HTTP/2 support. As of July 2023, approximately 97% of tracked desktop browsers support the protocol, though only 36% of the top 10 million websites implement it.

Extensive server support includes Apache (via mod_http2), Nginx 1.9.5+, Caddy, HAProxy, Jetty, LiteSpeed, and Microsoft IIS on Windows 10 and later server editions. Major CDN providers--Akamai, AWS CloudFront, Cloudflare, and Fastly--offer HTTP/2 support including Server Push functionality.

## Successor Protocol

HTTP/3 represents the next evolution, building upon HTTP/2's conceptual framework while addressing remaining performance limitations through QUIC transport protocol innovations.
