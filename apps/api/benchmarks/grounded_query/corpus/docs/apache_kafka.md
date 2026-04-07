# Apache Kafka

Apache Kafka is a distributed event store and stream-processing platform developed by the Apache Software Foundation as open-source software written in Java and Scala. Originally created at LinkedIn, it was open-sourced in January 2011 and graduated from the Apache Incubator on October 23, 2012.

## Overview

The platform aims to deliver a unified, high-throughput, low-latency platform for handling real-time data feeds. Kafka employs a binary TCP-based protocol optimized for efficiency, using message set abstractions that batch messages together. This design converts a bursty stream of random message writes into linear writes, improving performance through larger network packets and sequential disk operations.

## History

Jay Kreps, Neha Narkhede, and Jun Rao co-created Kafka at LinkedIn. Kreps selected the name after author Franz Kafka because the system was "optimized for writing," and he admired Kafka's literary work.

## Architecture & Operation

Kafka functions as a distributed log-based messaging system guaranteeing ordering within individual partitions rather than across entire topics. Unlike queue-based systems, it retains messages in a durable, append-only log, enabling multiple consumers to read from different offsets.

The system uses manual offset management, granting consumers authority over retries and failure handling. If message processing fails, consumers can delay offset commits, preventing partition progress while other partitions continue operating independently.

### Recent Enhancement

In 2025, Kafka introduced "Queues for Kafka" through share groups, offering queue-like semantics as an alternative to consumer groups. This allows consumers to cooperatively process records with individual message acknowledgment and delivery tracking, supporting scenarios where consumer counts exceed partition counts.

## APIs

**Connect API**: Added in version 0.9.0.0, this framework imports and exports data from external systems using Producer and Consumer APIs internally. Connectors implement logic for reading and writing data.

**Streams API**: Introduced in version 0.10.0.0, this Java stream-processing library enables stateful, scalable, fault-tolerant applications. It provides a domain-specific language with operators including filter, map, grouping, windowing, aggregation, and joins. The system uses RocksDB for maintaining local operator state, allowing state larger than available memory. State updates are written to Kafka cluster topics for fault-tolerance recovery.

## Technical Specifications

- **Current Release**: 4.1.1 (November 12, 2025)
- **License**: Apache License 2.0
- **Repository**: github.com/apache/kafka
- **Website**: kafka.apache.org
