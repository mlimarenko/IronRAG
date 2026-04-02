# Graph database

Source: https://en.wikipedia.org/wiki/Graph_database
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

A graph database (GDB) is a database that uses graph structures for semantic queries
with nodes, edges, and properties to represent and store data. A key concept of the
system is the graph (or edge or relationship). The graph relates the data items in the
store to a collection of nodes and edges, the edges representing the relationships
between the nodes. The relationships allow data in the store to be linked together
directly and, in many cases, retrieved with one operation. Graph databases hold the
relationships between data as a priority. Querying relationships is fast because they
are perpetually stored in the database. Relationships can be intuitively visualized
using graph databases, making them useful for heavily inter-connected data.
Graph databases are commonly referred to as a NoSQL database. Graph databases are
similar to 1970s network model databases in that both represent general graphs, but
network-model databases operate at a lower level of abstraction and lack easy traversal
over a chain of edges.
The underlying storage mechanism of graph databases can vary. Relationships are
first-class citizens in a graph database and can be labeled, directed, and given
properties. Some depend on a relational engine and store the graph data in a table
(although a table is a logical element, therefore this approach imposes a level of
abstraction between the graph database management system and physical storage devices).
Others use a key–value store or document-oriented database for storage, making them
inherently NoSQL structures.
As of 2021, no graph query language has been universally adopted in the same way as SQL
was for relational databases. There is a wide variety of systems used, many of which are
tightly tied to one product. Early standardization efforts led to multi-vendor query
languages like Gremlin, SPARQL, and Cypher. In September 2019 a proposal for a project
to create a new standard graph query language (ISO/IEC 39075 Information Technology —
Database Languages — GQL) was approved by members of ISO/IEC Joint Technical Committee
1(ISO/IEC JTC 1). GQL is intended to be a declarative database query language, like SQL.
In addition to having query language interfaces, some graph databases are accessed
through application programming interfaces (APIs).
Graph databases differ from graph compute engines. Graph databases are technologies that
are translations of the relational online transaction processing (OLTP) databases. On
the other hand, graph compute engines are used in online analytical processing (OLAP)
for bulk analysis. Graph databases attracted considerable attention in the 2000s, due to
the successes of major technology corporations using proprietary graph databases, along
with the introduction of open-source graph databases.
One study concluded that an RDBMS was "comparable" in performance to existing graph
analysis engines at executing graph queries.


== History ==
In the mid-1960s, navigational databases such as IBM's IMS supported tree-like
structures in its hierarchical model, but the strict tree structure could be
circumvented with virtual records.
Graph structures could be represented in network model databases from the late 1960s.
CODASYL, which had defined COBOL in 1959, defined the Network Database Language in 1969.
Labeled graphs could be represented in graph databases from the mid-1980s, such as the
Logical Data Model.
Commercial object databases (ODBMSs) emerged in the early 1990s. In 2000, the Object
Data Management Group published a standard language for defining object and relationship
(graph) structures in their ODMG'93 publication.
Several improvements to graph databases appeared in the early 1990s, accelerating in the
late 1990s with endeavors to index web pages.
In the mid-to-late 2000s, commercial graph databases with ACID guarantees such as Neo4j
and Oracle Spatial and Graph became available.
In the 2010s, commercial ACID graph databases that could be scaled horizontally became
available. Further, SAP HANA brought in-memory and columnar technologies to graph
databases. Also in the 2010s, multi-model databases that supported graph models (and
other models such as relational database or document-oriented database) became
available, such as OrientDB, ArangoDB, and MarkLogic (starting with its 7.0 version).
During this time, graph databases of various types have become especially popular with
social network analysis with the advent of social media companies. Also during the
decade, cloud-based graph databases such as Amazon Neptune and Neo4j AuraDB became
available.


== Background ==

Graph databases portray the data as it is viewed conceptually. This is accomplished by
transferring the data into nodes and its relationships into edges.
A graph database is a database that is based on graph theory. It consists of a set of
objects, which can be a node or an edge.

Nodes represent entities or instances such as people, businesses, accounts, or any other
item to be tracked. They are roughly the equivalent of a record, relation, or row in a
relational database, or a document in a document-store database.
Edges, also termed graphs or relationships, are the lines that connect nodes to other
nodes; representing the relationship between them. Meaningful patterns emerge when
examining the connections and interconnections of nodes, properties and edges. The edges
can either be directed or undirected. In an undirected graph, an edge connecting two
nodes has a single meaning. In a directed graph, the edges connecting two different
nodes have different meanings, depending on their direction. Edges are the key concept
in graph databases, representing an abstraction that is not directly implemented in a
relational model or a document-store model.
Properties are information associated to nodes. For example, if Wikipedia were one of
the nodes, it might be tied to properties such as website, reference material, or words
that starts with the letter w, depending on which aspects of Wikipedia are germane to a
given database.


== Graph models ==


=== Labeled-property graph ===

A labeled-property graph model is represented by a set of nodes, relationships,
properties, and labels. Both nodes of data and their relationships are named and can
store properties represented by key–value pairs. Nodes can be labelled to be grouped.
The edges representing the relationships have two qualities: they always have a start
node and an end node, and are directed; making the graph a directed graph. Relationships
can also have properties. This is useful in providing additional metadata and semantics
to relationships of the nodes. Direct storage of relationships allows a constant-time
traversal.


=== Resource Description Framework (RDF) ===

In an RDF graph model, each addition of information is represented with a separate node.
For example, imagine a scenario where a user has to add a name property for a person
represented as a distinct node in the graph. In a labeled-property graph model, this
would be done with an addition of a name property into the node of the person. However,
in an RDF, the user has to add a separate node called hasName connecting it to the
original person node. Specifically, an RDF graph model is composed of nodes and arcs. An
RDF graph notation or a statement is represented by: a node for the subject, a node for
the object, and an arc for the predicate. A node may be left blank, a literal and/or be
identified by a URI. An arc may also be identified by a URI. A literal for a node may be
of two types: plain (untyped) and typed. A plain literal has a lexical form and
optionally a language tag. A typed literal is made up of a string with a URI that
identifies a particular datatype. A blank node may be used to accurately illustrate the
state of the data when the data does not have a URI.


== Properties ==
Graph databases are a powerful tool for graph-like queries. For example, computing the
shortest path between two nodes in the graph. Other graph-like queries can be performed
over a graph database in a natural way (for example graph's diameter computations or
community detection).
Graphs are flexible, meaning it allows the user to insert new data into the existing
graph without loss of application functionality. There is no need for the designer of
the database to plan out extensive details of the database's future use cases.


=== Storage ===
The underlying storage mechanism of graph databases can vary. Some depend on a
relational engine and "store" the graph data in a table (although a table is a logical
element, therefore this approach imposes another level of abstraction between the graph
database, the graph database management system and the physical devices where the data
is actually stored). Others use a key–value store or document-oriented database for
storage, making them inherently NoSQL structures. A node would be represented as any
other document store, but edges that link two different nodes hold special attributes
inside its document; a _from and _to attributes.


=== Index-free adjacency ===
Data lookup performance is dependent on the access speed from one particular node to
another. Because index-free adjacency enforces the nodes to have direct physical RAM
addresses and physically point to other adjacent nodes, it results in a fast retrieval.
A native graph system with index-free adjacency does not have to move through any other
type of data structures to find links between the nodes. Directly related nodes in a
graph are stored in the cache once one of the nodes are retrieved, making the data
lookup even faster than the first time a user fetches a node. However, such advantage
comes at a cost. Index-free adjacency sacrifices the efficiency of queries that do not
use graph traversals. Native graph databases use index-free adjacency to process CRUD
operations on the stored data.


== Applications ==
Multiple categories of graphs by kind of data have been recognised. Gartner suggests the
five broad categories of graphs:

Social graph: this is about the connections between people; examples include Facebook,
Twitter, and the idea of six degrees of separation
Intent graph: this deals with reasoning and motivation.
Consumption graph: also known as the "payment graph", the consumption graph is heavily
used in the retail industry. E-commerce companies such as Amazon, eBay and Walmart use
consumption graphs to track the consumption of individual customers.
Interest graph: this maps a person's interests and is often complemented by a social
graph. It has the potential to follow the previous revolution of web organization by
mapping the web by interest rather than indexing webpages.
Mobile graph: this is built from mobile data. Mobile data in the future may include data
from the web, applications, digital wallets, GPS, and Internet of Things (IoT) devices.


== Comparison with relational databases ==
Since Edgar F. Codd's 1970 paper on the relational model, relational databases have been
the de facto industry standard for large-scale data storage systems. Relational models
require a strict schema and data normalization which separates data into many tables and
removes any duplicate data within the database. Data is normalized in order to preserve
data consistency and support ACID transactions. However this imposes limitations on how
relationships can be queried.
One of the relational model's design motivations was to achieve a fast row-by-row
access. Problems arise when there is a need to form complex relationships between the
stored data. Although relationships can be analyzed with the relational model, complex
queries performing many join operations on many different attributes over several tables
are required. In working with relational models, foreign key constraints should also be
considered when retrieving relationships, causing additional overhead.
Compared with relational databases, graph databases are often faster for associative
data sets and map more directly to the structure of object-oriented applications. They
can scale more naturally to large datasets as they do not typically need join
operations, which can often be expensive. As they depend less on a rigid schema, they
are marketed as more suitable to manage ad hoc and changing data with evolving schemas.
Conversely, relational database management systems are typically faster at performing
the same operation on large numbers of data elements, permitting the manipulation of the
data in its natural structure. Despite the graph databases' advantages and recent
popularity over  relational databases, it is recommended the graph model itself should
not be the sole reason to replace an existing relational database. A graph database may
become relevant if there is an evidence for performance improvement by orders of
magnitude and lower latency.


=== Examples ===
The relational model gathers data together using information in the data. For example,
one might look for all the "users" whose phone number contains the area code "311". This
would be done by searching selected datastores, or tables, looking in the selected phone
number fields for the string "311". This can be a time-consuming process in large
tables, so relational databases offer indexes, which allow data to be stored in a
smaller sub-table, containing only the selected data and a unique key (or primary key)
of the record. If the phone numbers are indexed, the same search would occur in the
smaller index table, gathering the keys of matching records, and then looking in the
main data table for the records with those keys. Usually, a table is stored in a way
that allows a lookup via a key to be very fast.
Relational databases do not inherently contain the idea of fixed relationships between
records. Instead, related data is linked to each other by storing one record's unique
key in another record's data. For example, a table containing email addresses for users
might hold a data item called userpk, which contains the primary key of the user record
it is associated with. In order to link users and their email addresses, the system
first looks up the selected user records primary keys, looks for those keys in the
userpk column in the email table (or, more likely, an index of them), extracts the email
data, and then links the user and email records to make composite records containing all
the selected data. This operation, termed a join, can be computationally expensive.
Depending on the complexity of the query, the number of joins, and indexing various
keys, the system may have to search through multiple tables and indexes and then sort it
all to match it together.
In contrast, graph databases directly store the relationships between records. Instead
of an email address being found by looking up its user's key in the userpk column, the
user record contains a pointer that directly refers to the email address record. That
is, having selected a user, the pointer can be followed directly to the email records,
there is no need to search the email table to find the matching records. This can
eliminate the costly join operations. For example, if one searches for all of the email
addresses for users in area code "311", the engine would first perform a conventional
search to find the users in "311", but then retrieve the email addresses by following
