# Transport Layer Security (TLS)

## Overview

Transport Layer Security is a cryptographic protocol enabling secure communications across computer networks. TLS is a cryptographic protocol designed to provide communications security over a computer network, such as the Internet.

The protocol operates at the presentation layer and comprises two components: the TLS record protocol and the TLS handshake protocol. It's predominantly recognized for securing HTTPS connections, though applications extend to email, instant messaging, and VoIP systems.

## Historical Development

### Early Predecessors

Research into transport layer security began in 1986 when government agencies launched the Secure Data Network System (SDNS) project, which later produced SP4--subsequently renamed the Transport Layer Security Protocol (TLSP) and published as an international standard in 1995.

Separate efforts included the Secure Network Programming (SNP) API developed by Professor Simon Lam at UT-Austin, which won the 2004 ACM Software System Award.

### SSL Evolution

Netscape developed the original SSL protocols, with Taher Elgamal recognized as the "father of SSL." Version 1.0 never reached public release due to critical flaws. SSL 2.0 (1995) contained substantial vulnerabilities, including "weak MAC construction that used the MD5 hash function" and provided no protection against man-in-the-middle attacks on handshakes.

SSL 3.0 (1996) represented a complete protocol redesign, produced by Paul Kocher working with Netscape engineers Phil Karlton and Alan Freier.

### TLS Versions

**TLS 1.0** (1999) provided an upgrade from SSL 3.0, though differences were incremental. Major improvements included explicit initialization vectors replacing implicit ones in TLS 1.1 (2006) to address cipher-block chaining vulnerabilities.

**TLS 1.2** (2008) replaced MD5/SHA-1 combinations with SHA-256, enhanced authenticated encryption support (particularly GCM and CCM modes), and expanded cipher suite options.

**TLS 1.3** (2018) introduced substantial changes:
- Mandated perfect forward secrecy through ephemeral keys
- Removed compression, renegotiation, and non-AEAD ciphers
- Added ChaCha20-Poly1305 and Ed25519/Ed448 algorithms
- Encrypted all handshake messages after ServerHello
- Reduced handshake latency through optimizations

Mozilla enabled TLS 1.3 by default in Firefox 60.0 (May 2018). OpenSSL 1.1.1 (September 2018) marked the protocol as its "headline new feature."

### Deprecation Timeline

- SSL 2.0: Deprecated March 2011
- SSL 3.0: Deprecated June 2015 following POODLE vulnerability discovery
- TLS 1.0 & 1.1: Deprecated March 2021 by major browser vendors

## Protocol Architecture

### Handshake Process

The TLS handshake establishes connection parameters through a multi-step negotiation:

1. Client requests secure connection, providing supported cipher suites
2. Server selects compatible cipher and hash function
3. Server presents digital certificate containing identity and public key
4. Client validates certificate authenticity
5. Parties exchange session keys via either:
   - RSA encryption of a random number using server's public key
   - Diffie-Hellman or elliptic-curve Diffie-Hellman key agreement (providing forward secrecy)

This process establishes a stateful connection using asymmetric cryptography for key exchange, followed by symmetric encryption for data transmission.

### Connection Properties

Secured TLS connections provide three essential properties:

**Privacy**: A symmetric-key algorithm is used to encrypt the data transmitted with unique keys negotiated per connection.

**Authentication**: The identity of the communicating parties can be authenticated using public-key cryptography, required for servers and optional for clients.

**Integrity**: Message authentication codes prevent undetected data loss or alteration during transmission.

## Datagram Transport Layer Security (DTLS)

DTLS adapts TLS for unreliable, datagram-oriented protocols including UDP and DCCP. The 2006 release provided deltas to TLS 1.1; DTLS 1.2 (2012) matched TLS 1.2 specifications; DTLS 1.3 (2022) targets equivalence with TLS 1.3 with the exception of order protection/non-replayability.

VPN clients from Cisco, OpenConnect, and Citrix utilize DTLS for UDP traffic security. Modern web browsers support DTLS-SRTP for WebRTC applications.

## Algorithms and Ciphers

### Key Exchange Methods

Supported approaches include RSA, Diffie-Hellman (DH), ephemeral DH (DHE), elliptic-curve variants (ECDH/ECDHE), pre-shared keys (PSK), and Secure Remote Password (SRP). Only DHE and ECDHE provide forward secrecy--a property where compromised long-term keys cannot decrypt previously recorded sessions.

TLS 1.3 exclusively supports forward-secrecy algorithms, eliminating static RSA and DH options.

### Cipher Suites

Modern secure ciphers include:
- AES-GCM (256/128-bit variants)
- ChaCha20-Poly1305
- Camellia-GCM

Deprecated/insecure options:
- RC4 (prohibited across all TLS versions)
- 3DES (112-bit effective strength below recommended 128-bit minimum)
- DES, IDEA (removed from TLS 1.2 onward)

### Data Integrity

HMAC provides message authentication for CBC mode ciphers. AEAD ciphers (GCM, CCM) integrate authentication without separate HMAC. TLS 1.3 mandates AEAD exclusively.

## Digital Certificates and Certificate Authorities

Certificates verify public key ownership and intended usage. Trust chains typically anchor in certificate authority (CA) lists distributed with user agents.

As of 2019, IdenTrust, DigiCert, and Sectigo dominate CA market share, replacing Symantec's historical leadership. The 2013 mass surveillance disclosures highlighted CAs as security weak points vulnerable to compromise or cooperation enabling man-in-the-middle attacks.

In April 2025, the CA/Browser Forum approved a ballot requiring public TLS certificate lifespans to gradually reduce to 47 days by 2029.

## Security Vulnerabilities and Attacks

### Notable Threats

**POODLE** (2014): Exploited SSL 3.0's block cipher vulnerability, prompting deprecation.

**BEAST** (Browser Exploit Against SSL/TLS): Affected CBC ciphers in SSL 3.0 and TLS 1.0 unless mitigated client-side.

**CRIME/BREACH**: Compression-based attacks exploiting data redundancy in encrypted HTTP traffic.

**Heartbleed**: An OpenSSL implementation bug (2014) allowing memory disclosure without authentication, affecting millions of servers.

**Sweet32**: Attacks 64-bit block ciphers (3DES, Blowfish) through birthday-bound collisions.

**RC4 Attacks**: Feasible cryptanalytic breaks render RC4 unsuitable despite historical use.

**FREAK/Logjam**: Downgrade attacks forcing use of weak export-grade ciphers.

**DROWN**: Cross-protocol attack exploiting SSL 2.0 support to compromise TLS sessions.

### Forward Secrecy

TLS 1.3 mandates perfect forward secrecy through ephemeral key agreement, ensuring session confidentiality survives long-term key compromise. Pre-TLS 1.3 versions support forward secrecy optionally via DHE/ECDHE algorithms.

### TLS Interception

Enterprise networks may deploy TLS interception for traffic analysis, typically via man-in-the-middle proxies holding trusted CA credentials. This practice conflicts with forward secrecy principles.

## Current Adoption Status

### Website Support (September 2025)

- TLS 1.3: 75.3% of surveyed sites
- TLS 1.2: 100% support (security varies by cipher selection)
- TLS 1.1: 25.2% legacy support
- TLS 1.0: 23.5% legacy support
- SSL 3.0: 1.0% legacy support

### Enterprise Transport Security

The ETSI TS103523-3 standard defines Enterprise Transport Security (ETS)--a TLS 1.3 variant intentionally disabling forward secrecy for proprietary network monitoring. The Electronic Frontier Foundation criticized this approach, noting "better ways to analyze traffic" exist while warning that loss of forward secrecy increases exposure risk.

## Implementation Libraries

Major cryptographic libraries include:
- OpenSSL (open-source reference implementation)
- Mozilla Network Security Services (NSS)
- wolfSSL (first commercial TLS 1.3 implementation, May 2017)
- Microsoft Secure Channel (schannel)

Windows 11 and Windows Server 2022 introduced TLS 1.3 support in Secure Channel GA releases.

## Protocol Ossification Challenge

TLS 1.3 deployment encountered middlebox incompatibility issues where network proxies rejected unrecognized version numbers. Resolution involved mimicking TLS 1.2's wire format while incrementing version internally--a late-stage discovery affecting protocol design philosophy and influencing subsequent standardization approaches like "greasing" extension points to resist ossification.
