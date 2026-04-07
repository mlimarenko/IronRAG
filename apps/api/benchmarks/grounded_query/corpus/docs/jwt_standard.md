# JSON Web Token

## Overview

**JSON Web Token** (JWT) is an Internet standard for creating data with optional signature and/or encryption whose payload contains JSON assertions known as claims. The tokens are signed using a private secret or public/private key pair.

A server can generate a token asserting claims like "logged in as administrator" and provide it to a client for authentication proof. The tokens are designed to be compact, URL-safe, and particularly useful in web-browser single-sign-on (SSO) contexts. JWT claims typically pass authenticated user identity between an identity provider and service provider, or convey other business-required claims.

JWT relies on two JSON-based standards: JSON Web Signature and JSON Web Encryption.

## Structure

A JWT consists of three encoded parts separated by periods:

### Header

Identifies the algorithm generating the signature. The example uses `HS256`, indicating HMAC-SHA256 signing:

```json
{
  "alg": "HS256",
  "typ": "JWT"
}
```

Common cryptographic algorithms include HMAC with SHA-256 (HS256) and RSA signature with SHA-256 (RS256).

### Payload

Contains a set of claims. The JWT specification defines seven registered claim names as standard fields. Custom claims are typically included based on token purpose:

```json
{
  "loggedInAs": "admin",
  "iat": 1422779638
}
```

### Signature

Securely validates the token. The signature is calculated by Base64url-encoding the header and payload, concatenating them with a period separator, then running them through the specified cryptographic algorithm:

```
HMAC_SHA256(
  secret,
  base64urlEncoding(header) + '.' +
  base64urlEncoding(payload)
)
```

The complete JWT combines all three encoded parts:

```
eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJsb2dnZWRJbkFzIjoiYWRtaW4iLCJpYXQiOjE0MjI3Nzk2Mzh9.gzSraSYS8EXBxLN_oWnFSRgCzcmJmMjLiuyu5CSpyHI
```

This token can be easily passed through HTML and HTTP.

## Use Cases

### Authentication Flow

Upon successful login, a JWT should be returned to the client via a secure mechanism like an HTTP-only cookie. Storing JWTs in browser storage (localStorage or sessionStorage) is discouraged, as client-side JavaScript can access these mechanisms and expose the token.

For cross-origin API authentication with HTTP-only cookies, use the fetch credentials property:

```javascript
fetch('https://api.example.com/data', {
  method: 'GET',
  credentials: 'include'
})
  .then(response => response.json())
  .then(data => console.log(data))
  .catch(error => console.error('Error:', error));
```

### Machine-to-Machine Authentication

For unattended processes, clients can generate and sign their own JWT with a pre-shared secret, passing it to an OAuth-compliant service:

```
POST /oauth2/token
Content-type: application/x-www-form-urlencoded

grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion=eyJhb...
```

The server responds with an access token:

```json
{
  "access_token": "eyJhb...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

### Protected Resource Access

When accessing protected routes, clients send the JWT in the Authorization HTTP header using Bearer schema:

```
Authorization: Bearer eyJhbGci...<snip>...yu5CSpyHI
```

This enables stateless authentication--the user state is never saved in server memory. Protected routes verify valid JWTs in the Authorization header, granting access to resources. Since JWTs are self-contained, all necessary information is present, reducing database query needs.

## Standard Fields

| Code | Name | Description |
|------|------|-------------|
| `iss` | Issuer | Identifies the principal issuing the JWT, such as an organization or website URL |
| `sub` | Subject | Identifies the JWT subject, such as a username or account number |
| `aud` | Audience | Identifies JWT recipients; each intended principal must identify itself with an audience claim value or the JWT must be rejected |
| `exp` | Expiration Time | Identifies expiration time; the JWT must not be accepted after this NumericDate (integer/decimal representing seconds past 1970-01-01 00:00:00Z) |
| `nbf` | Not Before | Identifies when the JWT starts being accepted for processing as a NumericDate |
| `iat` | Issued at | Identifies JWT issuance time as a NumericDate |
| `jti` | JWT ID | Case-sensitive unique token identifier, even among different issuers |

### Common Header Fields

| Code | Name | Description |
|------|------|-------------|
| `typ` | Token type | Must be set to a registered IANA Media Type if present |
| `cty` | Content type | Recommended to set to `JWT` for nested signing/encryption; otherwise omit |
| `alg` | Message authentication code algorithm | Freely set by the issuer to verify signature; some algorithms are insecure |
| `kid` | Key ID | Hint indicating which key generated the token signature; server matches this to verify authenticity |
| `x5c` | x.509 Certificate Chain | RFC4945-formatted certificate chain for the private key; server uses this to verify signature validity |
| `x5u` | x.509 Certificate Chain URL | URL where the server retrieves the certificate chain for the private key |
| `crit` | Critical | List of headers that must be understood by the server to accept the token as valid |

## Implementations

JWT implementations exist across numerous languages and frameworks, including .NET, C, C++, Clojure, Common Lisp, Dart, Elixir, Erlang, Go, Haskell, Java, JavaScript, Julia, Lua, Node.js, OCaml, Perl, PHP, PL/SQL, PowerShell, Python, Racket, Raku, Ruby, Rust, Scala, and Swift.

## Vulnerabilities

### Session Invalidation Limitations

JWTs may contain session state. However, if project requirements mandate session invalidation before JWT expiration, services cannot rely solely on token assertions. Token assertions must be checked against a data store to validate sessions aren't revoked, rendering tokens no longer stateless and undermining JWT's primary advantage.

### Algorithm Confusion

Security consultant Tim McLean identified vulnerabilities in some JWT libraries that misused the `alg` field to validate tokens, most commonly accepting `alg=none` tokens. While patched, McLean suggested deprecating the `alg` field entirely. New `alg=none` vulnerabilities continue appearing, with four CVEs filed during 2018-2021 from this cause.

### Mitigation Strategies

Developers can address algorithm vulnerabilities through proper design:

1. Never let the JWT header alone drive verification
2. Know the algorithms; avoid depending solely on the `alg` field
3. Use appropriate key sizes

### Elliptic-Curve Attacks

Several JWT libraries proved vulnerable to invalid elliptic-curve attacks in 2017.

### Design Complexity Concerns

Some security experts contend that JSON web tokens are challenging to implement securely due to the numerous encryption algorithms and options available in the standard, advocating alternative standards for both web frontends and backends.
