# PostgreSQL

## Overview

PostgreSQL is a free and open-source relational database management system (RDBMS) emphasizing extensibility and SQL compliance. It provides transaction support with ACID properties, automatically updatable views, materialized views, triggers, foreign keys, and stored procedures across all major operating systems.

## History

### Origins (1982-1994)

PostgreSQL evolved from the Ingres project at UC Berkeley. Michael Stonebraker led the original Ingres team before creating a proprietary version in 1982. He returned to Berkeley in 1985 to address problems with contemporary database systems that had become increasingly clear during the early 1980s.

The POSTGRES project aimed to add minimal features needed to support data types with relationship understanding built into the database. Starting in 1986, published papers described the system basis, with a prototype shown at the 1988 ACM SIGMOD Conference. Berkeley released POSTGRES under an MIT License variant, enabling broader developer adoption.

### Postgres95 Era (1994-1996)

In 1994, Berkeley graduate students Andrew Yu and Jolly Chen replaced the POSTQUEL query language with SQL, creating Postgres95. The first version (0.01) was announced to beta testers on May 5, 1995, with version 1.0 released September 5, 1995, featuring a more liberal license that enabled the software to be freely modifiable.

On July 8, 1996, Marc Fournier provided the first non-university development server for open-source efforts. With Bruce Momjian and Vadim B. Mikheev's participation, stabilization work began on the inherited Berkeley codebase.

### PostgreSQL (1996-Present)

The project was renamed PostgreSQL in 1996 to reflect SQL support. The PostgreSQL.org website launched October 22, 1996, with version 6.0 released January 29, 1997. Since then developers and volunteers around the world have maintained the software as The PostgreSQL Global Development Group.

As of 2025, PostgreSQL is on major release version 18, notable in implementing asynchronous I/O (AIO), enabling database users to perform concurrent I/O tasks like readahead and sequential scan.

## Multiversion Concurrency Control (MVCC)

PostgreSQL manages concurrency through MVCC, which gives each transaction a "snapshot" of the database, allowing changes to be made without affecting other transactions. This approach largely eliminates read lock requirements while maintaining ACID principles.

PostgreSQL offers four transaction isolation levels: Read Uncommitted, Read Committed, Repeatable Read, and Serializable. The system is immune to dirty reads, requesting a Read Uncommitted transaction isolation level provides read committed instead. Full serializability is supported via the serializable snapshot isolation (SSI) method, though the PostgreSQL MVCC implementation is prone to performance issues that require tuning when under a heavy write load which updates existing rows.

## Storage and Replication

### Replication Features

PostgreSQL includes built-in binary replication based on shipping the changes (write-ahead logs (WAL)) to replica nodes asynchronously, with the ability to run read-only queries against these replicated nodes.

Synchronous replication ensures for each write transaction, the master waits until at least one replica node has written the data to its transaction log. Transaction durability can be specified per-database, per-user, per-session or even per-transaction, allowing flexibility for different workload requirements.

Standby servers can operate synchronously or asynchronously, with synchronous servers specified in configuration to determine synchronous replication candidates.

### Indexes

PostgreSQL includes built-in B-tree and hash table indexes, plus four access methods: generalized search trees (GiST), generalized inverted indexes (GIN), Space-Partitioned GiST (SP-GiST), and Block Range Indexes (BRIN). User-defined index methods can be created.

Advanced indexing features include:
- Expression indexes on function results
- Partial indexes using WHERE clauses
- Multi-index queries using bitmap operations
- k-nearest neighbors (k-NN) indexing for similarity searching
- Index-only scans avoiding main table access

### Schemas

PostgreSQL schemas are namespaces, allowing objects of the same kind and name to co-exist in a single database. A `search_path` setting determines the order in which PostgreSQL checks schemas for unqualified objects, defaulting to `$user, public`.

### Data Types

PostgreSQL supports extensive native data types:
- Boolean, arbitrary-precision numerics, text variants
- Date/time with timezone options
- Geometric primitives, IPv4/IPv6, CIDR blocks, MAC addresses
- XML with XPath support, UUID
- JSON and faster binary JSONB
- Arrays up to 1 GB storage
- Custom types via PostGIS and other extensions

Range types support discrete and continuous ranges with inclusive/exclusive boundaries using [] and () syntax.

### User-Defined Objects

Users can create custom casts, conversions, data types, domains, functions (including aggregates and window functions), indexes, operators, and procedural languages.

### Inheritance

Tables can be set to inherit their characteristics from a parent table. Data in child tables will appear to exist in the parent tables, unless data is selected from the parent table using the ONLY keyword.

Inheritance enables table partitioning and mapping entity-relationship diagram generalization hierarchies, though table constraints are not currently inheritable.

### Additional Storage Features

PostgreSQL provides referential integrity constraints, foreign keys, binary/textual large-object storage, tablespaces, per-column collation, online backup, point-in-time recovery via write-ahead logging, and in-place upgrades via pg_upgrade.

## Control and Connectivity

### Foreign Data Wrappers

PostgreSQL can link to other systems to retrieve data via foreign data wrappers (FDWs). These access various data sources--file systems, RDBMSs, web services--allowing queries to use them as regular tables with cross-source joins.

### Interfaces

PostgreSQL supports a binary communication protocol that allows applications to connect to the database server. The protocol is versioned (currently 3.0, as of PostgreSQL 7.4).

The official client implementation is libpq, a C API. ECPG allows embedding SQL in C code. Third-party libraries exist for C++, Java, Julia, Python, Node.js, Go, and Rust.

### Procedural Languages

Procedural languages extend the database with custom subroutines for triggers, custom data types, and aggregate functions. Safe language procedures are sandboxed; unsafe languages require superuser privileges but access external resources.

PostgreSQL has built-in support for:
- Plain SQL (safe), with inline expansion optimization
- PL/pgSQL (safe), resembling Oracle's PL/SQL
- C (unsafe), offering best performance but requiring code safety

Extensions support Perl, Tcl, and Python. External projects provide PL/Java, JavaScript, Julia, R, Ruby, and others.

### Triggers

Triggers are events triggered by the action of SQL data manipulation language (DML) statements. They are fully supported and can be attached to tables. Triggers can be per-column and conditional, with UPDATE triggers targeting specific columns. Multiple triggers execute in alphabetical order, and INSTEAD OF conditions attach triggers to views.

### Asynchronous Notifications

PostgreSQL provides an asynchronous messaging system that is accessed through the NOTIFY, LISTEN and UNLISTEN commands. Sessions detect events via LISTEN commands, reducing polling overhead. Notifications are fully transactional, in that messages are not sent until the transaction they were sent from is committed.

The system functions as an effective, persistent pub/sub server or job server by combining LISTEN with FOR UPDATE SKIP LOCKED.

### Rules

Rules enable query tree rewriting of incoming queries, functioning as an automatically invoked macro language for SQL. They are attached to a table/class and re-write the incoming DML (select, insert, update, and/or delete). Query re-writing occurs after DML statement parsing and before query planning.

### Query Features

PostgreSQL supports transactions, full-text search, materialized/updateable/recursive views, all join types, sub-selects with correlation, regular expressions, common table expressions, TLS encryption, domains, savepoints, two-phase commits, TOAST compression, and embedded SQL via ECPG.

### Concurrency Model

PostgreSQL uses a process-based (not threaded) server model, using one operating system process per database session. Multiple sessions are automatically spread across all available CPUs by the operating system. Many queries parallelize across multiple background worker processes, and client applications can use threads and create multiple database connections from each thread.

## Security

PostgreSQL manages security on a per-role basis. A role is generally regarded to be a user (a role that can log in), or a group. Permissions apply down to the column level and control visibility/creation/alteration/deletion of objects at the database, schema, table, and row levels.

The SECURITY LABEL feature allows for additional security; with a bundled loadable module that supports label-based mandatory access control (MAC) based on Security-Enhanced Linux (SELinux) security policy.

PostgreSQL supports SCRAM-SHA-256, MD5, and plaintext password authentication, plus GSSAPI, SSPI, Kerberos, ident, peer, LDAP/Active Directory, RADIUS, certificate, and PAM authentication.

The host-based authentication configuration file (pg_hba.conf) determines what connections are allowed. This allows control over which user can connect to which database, where they can connect from (IP address, IP address range, domain socket).

## Standards Compliance

PostgreSQL claims high, but not complete, conformance with the latest SQL standard. As of version 17 (September 2024), PostgreSQL conforms to at least 170 of the 177 mandatory features for SQL:2023 Core conformance, with no other databases fully conforming to it.

One exception is identifier handling: unquoted identifiers fold to lowercase, whereas standards require uppercase folding. Temporal tables with automatic row version logging remain absent.

## Performance and Benchmarks

Performance improvements began heavily with version 8.1. Benchmarks showed version 8.4 was more than ten times faster on read-only workloads and at least 7.5 times faster on both read and write workloads compared to version 8.0.

In June 2007, a Sun-based benchmark achieved 778.14 SPECjAppServer2004 JOPS@Standard, comparing favorably with Oracle 10 on Itanium systems. An August 2007 improved score reached 813.73 JOPS@Standard with better price/performance.

In April 2012, EnterpriseDB demonstrated PostgreSQL 9.2's linear CPU scalability using a server with 64 cores. Genomic data benchmarking showed PostgreSQL extracting overlapping regions eight times faster than MySQL.

Default configuration uses only a small amount of dedicated memory for performance-critical purposes such as caching database blocks and sorting.

## Supported Platforms

PostgreSQL runs on Linux (all distributions), macOS 10.14+, Windows (64-bit Server 2022 and 2016), FreeBSD, OpenBSD, NetBSD, DragonFlyBSD, Solaris, and illumos.

Supported architectures include 64-bit x86-64, 32-bit x86, 64-bit ARM, 32-bit ARM (including Raspberry Pi), RISC-V, z/Architecture, S/390, PowerPC, SPARC, MIPS, and PA-RISC.

## Database Administration Tools

### Built-in Tools

**psql**: The primary command-line interface supporting SQL queries directly, or execute them from a file with meta-commands and shell-like features including tab completion.

### Open Source GUI Tools

**pgAdmin**: A free and open-source graphical user interface (GUI) administration tool for PostgreSQL, which is supported on many computer platforms. Available in numerous languages, pgAdmin III was written in C++ using wxWidgets. pgAdmin 4, released 2016, uses Python, Flask, and the Qt framework for web-based deployment.

**phpPgAdmin**: A web-based administration tool for PostgreSQL written in PHP and based on the popular phpMyAdmin interface.

**PostgreSQL Studio**: Enables essential PostgreSQL database development tasks from a web-based console allowing work with cloud databases without the need to open firewalls.

**DBeaver**: A free and open source GUI administration tool for PostgreSQL featuring visual entity diagrams and Intellisense.

**Adminer**: A simple web-based administration tool for PostgreSQL and others, written in PHP.

### Specialized Tools

**pgBadger**: A PostgreSQL log analyzer generating detailed reports.

**pgDevOps**: A web tool suite for installing/managing versions, developing SQL queries, and monitoring.

**pgBackRest**: Provides backup/restore with full, differential, and incremental support.

**pgaudit**: An extension providing detailed session and/or object audit logging via the standard logging facility.

**WAL-E**: A Python-based backup/restore tool for WAL-based backups.

**Postgresus**: An open-source backup tool with external storage (S3, NAS, FTP, Google services) and notification support.

**TeamPostgreSQL**: An AJAX/JavaScript web interface with SQL editor autocompletion, row editing, and SSH support.

## Notable Users

Organizations using PostgreSQL as primary database include:

- **Microsoft**: Uses PostgreSQL for petabyte-scale "Release Quality View" analytics dashboards tracking Windows update quality across 800M+ devices.
- **Instagram**: Mobile photo-sharing service
- **Disqus**: Online discussion and commenting service
- **TripAdvisor**: Travel-information website
- **Yandex**: Switched Yandex.Mail from Oracle to PostgreSQL
- **Amazon Redshift**: Columnar OLAP system based on PostgreSQL modifications
- **OpenAI**: Uses PostgreSQL for primary API service including ChatGPT
- **The Guardian**: Migrated from MongoDB to PostgreSQL in 2018
- **Reddit, Skype, MusicBrainz, OpenStreetMap, Afilias**

## Service Implementations

Notable SaaS providers offering PostgreSQL:

**Heroku**: A platform-as-a-service provider supporting PostgreSQL since 2007, offering features like full database roll-back (ability to restore a database from any specified time) based on WAL-E, open-source software developed by Heroku.

**EnterpriseDB**: Offers cloud versions alongside proprietary tools for administration, modeling, importing, exporting, and reporting.
