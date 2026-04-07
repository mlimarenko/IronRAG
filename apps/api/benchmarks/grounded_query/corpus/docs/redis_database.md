# Redis

## Overview

Redis (Remote Dictionary Server) is an in-memory key-value database functioning as a distributed cache and message broker with optional durability. By storing all data in memory, it provides low-latency reads and writes, making it particularly suitable for use cases that require a cache.

## Development History

Salvatore Sanfilippo created Redis in 2009 while attempting to improve his Italian startup's scalability. He initially prototyped the system in Tcl, then translated it to C. After internal success, Sanfilippo open-sourced the project on Hacker News, gaining early adoption from GitHub and Instagram.

**Key Timeline:**
- **March 2010**: VMware hired Sanfilippo
- **May 2013**: Pivotal Software (VMware spin-off) sponsored development
- **June 2015**: Redis Ltd. became the sponsor
- **October 2018**: Redis 5.0 released with Stream data structure
- **June 2020**: Sanfilippo stepped down; succeeded by Yossi Gottlieb and Oran Agra
- **March 2024**: Core Redis relicensed to RSAL and SSPL (non-free)
- **May 2025**: Added AGPL as third license option
- **December 2024**: Sanfilippo returned to the project

## Technical Characteristics

### Architecture

Redis differs fundamentally from relational databases by using a data model that differs from relational database management system approaches. Instead of queries executed by engines, commands specify operations on abstract data types. Data structures are designed for direct retrieval without secondary indexes or aggregations.

The system uses the fork system call to duplicate processes, allowing the parent process to continue serving clients while the child process persists the in-memory data to disk.

### Data Types

Redis supports multiple data structures:
- Strings
- JSON documents
- Hashes (field name-value pairs)
- Lists
- Sets
- Vector sets

As of May 1, 2025, all data types are included in the same package for Redis 8.0+ under unified licensing.

### Core Features

**Query Engine**: Enables Redis to function as a document database, vector database, secondary index, and search engine with support for vector search, full-text search, geospatial queries, and aggregations.

**Pub/Sub**: A lightweight messaging capability where publishers send messages to channels and subscribers receive them.

**Transactions**: Groups commands executing as a single isolated operation without interruption from other clients.

**Lua Scripting**: Users can upload and execute Lua scripts on the server.

## Persistence Mechanisms

Redis offers two persistence approaches:

1. **Snapshotting**: Asynchronously transfers datasets from memory to disk at intervals using RDB Dump File Format

2. **Append-Only File (AOF)**: Records each dataset-modifying operation in a background process, considered the safer approach since introduction in version 1.1

By default, data writes occur at least every two seconds, resulting in minimal data loss during system failures.

## Replication

Redis implements master-replica replication allowing data replication to any number of replicas. A replica may serve as master to another replica, creating single-rooted replication trees. Replicas can accept writes, permitting intentional inconsistency. This architecture supports read (but not write) scalability or data redundancy.

## Performance Characteristics

Redis operates as a single process and is single-threaded or double-threaded when it rewrites the AOF. This single-threaded design prevents parallel task execution but enables high performance for durability-optional workloads compared to disk-writing systems.

## Clustering

Introduced in April 2015 with version 3.0, Redis clustering implements a subset of Redis commands: single-key commands are available; multi-key operations restrict to same-node keys; database selection commands are unavailable. Clusters scale to 1,000 nodes while maintaining write safety and fault tolerance.

## Use Cases

Typical applications include:
- Session caching
- Full page cache
- Message queue applications
- Leaderboards
- Counting operations

Real-time server communication leverages publish-subscribe messaging paradigms.

## Enterprise Adoption

Redis users include Adobe, Airbnb, Amazon, Hulu, OpenAI, Salesforce, Shopify, Tinder, Twitter, and Yahoo. Major cloud providers offer managed services: AWS ElastiCache, Google Cloud Memorystore, Microsoft Azure Cache, and Alibaba ApsaraDB.

## Licensing Evolution

Redis experienced significant licensing changes:

- **Original**: BSD-3 license (2009-2024)
- **August 2018**: Modules adopted Apache 2.0 with Commons Clause restrictions
- **February 2019**: Switched to Redis Source Available License explicitly prohibiting commercial module use in databases, caching engines, and ML serving
- **March 2024**: Core Redis moved to dual licensing (RSAL and SSPL)
- **May 2025**: Transitioned to tri-licensing including AGPL

The 2024 license change prompted the Linux Foundation to fork the last BSD-licensed version as Valkey.
