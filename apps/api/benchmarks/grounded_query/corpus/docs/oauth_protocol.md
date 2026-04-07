# OAuth

## Overview

**OAuth** (short for "open authorization") is an open standard for access delegation. It enables internet users to grant websites or applications access to their information on other platforms without sharing passwords. Major companies including Amazon, Google, Meta Platforms, Microsoft, and Twitter employ OAuth to allow users to share account information with third-party applications.

The protocol provides resource owners with a method to authorize third-party clients for secure delegated access to server resources. Through HTTP, OAuth allows authorization servers to issue access tokens to third-party clients with resource owner approval. Third parties then use these tokens to access protected resources hosted by resource servers.

## Historical Development

OAuth originated in November 2006 when Blaine Cook developed an OpenID implementation for Twitter while Ma.gnolia needed a solution for OpenID members to authorize Mac OS X Dashboard widgets. Cook, Chris Messina, and Larry Halff from Magnolia collaborated with David Recordon to explore using OpenID with Twitter and Magnolia APIs for delegation authentication. They determined no open standards existed for API access delegation.

An OAuth discussion group formed in April 2007 with implementers drafting an open protocol proposal. DeWitt Clinton from Google expressed interest in supporting the effort. By July 2007, the team produced an initial specification. Eran Hammer later coordinated contributions, creating a more formal specification. The OAuth Core 1.0 final draft released on October 3, 2007.

At the 73rd Internet Engineering Task Force (IETF) meeting in Minneapolis in November 2008, an OAuth Birds of a Feather session occurred to discuss bringing the protocol into the IETF for standardization work. The well-attended event generated wide support for formally chartering an OAuth working group within the IETF.

OAuth 1.0 was published as RFC 5849 (informational Request for Comments) in April 2010. From August 31, 2010 onward, all third-party Twitter applications required OAuth usage.

OAuth 2.0 emerged based on additional use cases and extensibility requirements from the broader IETF community. Though built on OAuth 1.0 deployment experience, OAuth 2.0 is not backwards compatible with its predecessor. Both RFC 6749 (OAuth 2.0 framework) and RFC 6750 (Bearer Token Usage specification) were published as standards track Requests for Comments in October 2012.

As of November 2024, the OAuth 2.1 Authorization Framework draft consolidates functionality from multiple RFCs: OAuth 2.0, OAuth 2.0 for Native Apps, Proof Key for Code Exchange, OAuth 2.0 for Browser-Based Apps, OAuth Security Best Current, and Bearer Token Usage.

## Security Issues

### OAuth 1.0

On April 23, 2009, a session fixation security flaw in the 1.0 protocol was announced, affecting the OAuth authorization flow (known as "3-legged OAuth") in OAuth Core 1.0 Section 6. Version 1.0a of OAuth Core protocol addressed this issue.

### OAuth 2.0

In January 2013, the IETF published a threat model for OAuth 2.0. Among outlined threats is "Open Redirector"; early 2014 saw a variant described as "Covert Redirect" by Wang Jing.

Formal web protocol analysis revealed that in setups with multiple authorization servers, where one behaves maliciously, clients may become confused about which authorization server to use and forward secrets to the malicious server (AS Mix-Up Attack). This prompted creating a new best current practice internet draft defining new OAuth 2.0 security standards. Under strong attacker models assuming AS Mix-Up Attack fixes, formal analysis has proven OAuth 2.0's security.

One OAuth 2.0 implementation exposed numerous security flaws. In April and May 2017, approximately one million Gmail users (less than 0.1% as of May 2017) faced an OAuth-based phishing attack via emails purporting to be from colleagues or employers sharing Google Docs. Victims directed to sign in allowed a potentially malicious third-party program called "Google Apps" to access email, contacts, and documents. Google stopped the attack within approximately one hour and advised users to revoke access and change passwords.

In the OAuth 2.1 draft, PKCE (RFC 7636) extension use is recommended for all OAuth client types, including web applications and confidential clients, to prevent malicious browser extensions from performing OAuth 2.0 code injection attacks.

## OAuth Grant Types

OAuth framework specifies several grant types for different use cases:

- Authorization Code
- PKCE
- Client Credentials
- Device Code
- Refresh Token
- Resource Owner Password Credentials (ROPC)

## Common Uses

Facebook's Graph API exclusively supports OAuth 2.0. Google supports OAuth 2.0 as the recommended authorization mechanism for all APIs. Microsoft also supports OAuth 2.0 for various APIs and Azure Active Directory service, securing many Microsoft and third-party APIs.

OAuth serves as an authorization mechanism for accessing secured RSS/Atom feeds requiring authentication. For example, an RSS feed from a secured Google Site could be accessed using three-legged OAuth to authorize the RSS client.

Free software client implementations like the LibreOffice OAuth2OOo extension enable access to remote resources via Google API or Microsoft Graph API and support HTTP requests with OAuth 2.0 protocol in LibreOffice macros.

## OAuth and Other Standards

OAuth is complementary to and distinct from OpenID. OAuth is unrelated to OATH (a reference architecture for authentication). However, OAuth directly relates to OpenID Connect (OIDC), which builds an authentication layer atop OAuth 2.0. OAuth is also unrelated to XACML (an authorization policy standard), though they work together--OAuth handles ownership consent and access delegation while XACML defines authorization policies.

### Authentication vs. Authorization Distinction

OAuth is an authorization protocol rather than an authentication protocol. Using OAuth alone as an authentication method may be called pseudo-authentication, which can create major security flaws since OAuth was not designed for this use case.

The communication flow between OpenID and OAuth is similar through steps involving identity provider interactions and redirects. The crucial difference: in OpenID authentication, the identity provider response asserts identity; in OAuth authorization, the response is an access token granting the application ongoing API access. The token acts as a "valet key" proving permission to access APIs.

### OAuth and XACML Integration

XACML is a policy-based, attribute-based access control authorization framework providing an access control architecture, policy language for expressing access control policies, and request/response schemes for authorization.

XACML and OAuth combine for comprehensive authorization approaches. OAuth lacks a policy language for defining access control policies, while XACML provides one. OAuth focuses on delegated access (users grant service A access to their service B account) and identity-centric authorization. XACML uses attribute-based approaches considering user, action, resource, and context attributes (who, what, where, when, how).

Examples of XACML policies:
- Managers can view documents in their department
- Managers can edit documents they own in draft mode

XACML provides finer-grained access control than OAuth, which limits granularity to coarse functionality the target service exposes. XACML works transparently across multiple stacks (APIs, web SSO, ESBs, custom apps, databases), while OAuth focuses exclusively on HTTP-based applications.

## Controversy

Eran Hammer resigned from his lead author role for OAuth 2.0 in July 2012, withdrew from the IETF working group, and removed his name from the specification. Hammer cited conflict between web and enterprise cultures, noting that the IETF community emphasizes "enterprise use cases" and is "not capable of simple." He characterized the offering as "a blueprint for an authorization protocol" providing "the enterprise way" and opening "a whole new frontier to sell consulting services and integration solutions."

Comparing OAuth 2.0 with OAuth 1.0, Hammer contended it became "more complex, less interoperable, less useful, more incomplete, and most importantly, less secure." He explained how architectural changes for 2.0 unbound tokens from clients, removed all signatures and cryptography at protocol level, and added expiring tokens while complicating authorization processing. Numerous items remained unspecified or unlimited because "no issue is too small to get stuck on or leave open for each implementation to decide."

David Recordon later also removed his name from specifications for unspecified reasons. Dick Hardt assumed the editor role, and the framework published in October 2012.

David Harris, author of Pegasus Mail email client, criticized OAuth 2.0 as "an absolute dog's breakfast," requiring developers to write custom modules specific to each service and register individually with them.
