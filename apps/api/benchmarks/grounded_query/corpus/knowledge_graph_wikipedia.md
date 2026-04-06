# Knowledge graph

Source: https://en.wikipedia.org/wiki/Knowledge_graph
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

In knowledge representation and reasoning, a knowledge graph is a knowledge base that
uses a graph-structured data model or topology to represent and operate on data.
Knowledge graphs are often used to store interlinked descriptions of entities –
objects, events, situations or abstract concepts –  while also encoding the free-form
semantics or relationships underlying these entities.
Since the development of the Semantic Web, knowledge graphs have often been associated
with linked open data projects, focusing on the connections between concepts and
entities. They are also historically associated with and used by search engines such as
Google, Bing, and Yahoo; knowledge engines and question-answering services such as
WolframAlpha, Apple's Siri, and Amazon Alexa; and social networks such as LinkedIn and
Facebook.
Recent developments in data science and machine learning, particularly in graph neural
networks and representation learning and also in machine learning, have broadened the
scope of knowledge graphs beyond their traditional use in search engines and recommender
systems. They are increasingly used in scientific research, with notable applications in
fields such as genomics, proteomics, and systems biology.


== History ==
The term was coined as early as 1972 by the Austrian linguist Edgar W. Schneider, in a
discussion of how to build modular instructional systems for courses. In the late 1980s,
the University of Groningen and University of Twente jointly began a project called
Knowledge Graphs, focusing on the design of semantic networks with edges restricted to a
limited set of relations, to facilitate algebras on the graph. In subsequent decades,
the distinction between semantic networks and knowledge graphs was blurred.
Some early knowledge graphs were topic-specific. In 1985, Wordnet was founded, capturing
semantic relationships between words and meanings – an application of this idea to
language itself. In 2005, Marc Wirk founded Geonames to capture relationships between
different geographic names and locales and associated entities. In 1998, Andrew Edmonds
of Science in Finance Ltd in the UK created a system called ThinkBase that offered
fuzzy-logic based reasoning in a graphical context.
In 2007, both DBpedia and Freebase were founded as graph-based knowledge repositories
for general-purpose knowledge. DBpedia focused exclusively on data extracted from
Wikipedia, while Freebase also included a range of public datasets. Neither described
themselves as a 'knowledge graph' but developed and described related concepts.
In 2012, Google introduced their Knowledge Graph, building on DBpedia and Freebase among
other sources. They later incorporated RDFa, Microdata, JSON-LD content extracted from
indexed web pages, including the CIA World Factbook, Wikidata, and Wikipedia. Entity and
relationship types associated with this knowledge graph have been further organized
using terms from the schema.org vocabulary. The Google Knowledge Graph became a
complement to string-based search within Google, and its popularity online brought the
term into more common use.
Since then, several large multinationals have advertised their use of knowledge graphs,
further popularising the term. These include Facebook, LinkedIn, Airbnb, Microsoft,
Amazon, Uber and eBay.
In 2019, IEEE combined its annual international conferences on "Big Knowledge" and "Data
Mining and Intelligent Computing" into the International Conference on Knowledge Graph.
The development of large language models expanded interest in knowledge graphs as a way
to structure information from unstructured text, with advances in language processing
enabling their automatic or semi-automatic generation and expansion. The term knowledge
graph has since broadened to include the dynamically constructed and adaptive graph
structures, which support retrieval, reasoning, and summarization in generative systems.
Microsoft Research's GraphRAG (2024) exemplified this development by integrating
LLM-generated graphs into retrieval-augmented generation.


== Definitions ==
There is no single commonly accepted definition of a knowledge graph. Most definitions
view the topic through a Semantic Web lens and include these features:

Flexible relations among knowledge in topical domains: A knowledge graph (i) defines
abstract classes and relations of entities in a schema, (ii) mainly describes real world
entities and their interrelations, organized in a graph, (iii) allows for potentially
interrelating arbitrary entities with each other, and (iv) covers various topical
domains.
General structure: A network of entities, their semantic types, properties, and
relationships. To represent properties, categorical or numerical values are often used.
Supporting reasoning over inferred ontologies: A knowledge graph acquires and integrates
information into an ontology and applies a reasoner to derive new knowledge.
There are, however, many knowledge graph representations for which some of these
features are not relevant.  For those knowledge graphs, this simpler definition may be
more useful:

A digital structure that represents knowledge as concepts and the relationships between
them (facts). A knowledge graph can include an ontology that allows both humans and
machines to understand and reason about its contents.


=== Implementations ===
In addition to the above examples, the term has been used to describe open knowledge
projects such as YAGO and Wikidata; federations like the Linked Open Data cloud; a range
of commercial search tools, including Yahoo's semantic search assistant Spark, Google's
Knowledge Graph, and Microsoft's Satori; and the LinkedIn and Facebook entity graphs.
The term is also used in the context of note-taking software applications that allow a
user to build a personal knowledge graph.
The popularization of knowledge graphs and their accompanying methods have led to the
development of graph databases such as Neo4j, GraphDB and AgensGraph. These graph
databases allow users to easily store data as entities and their interrelationships, and
facilitate operations such as data reasoning, node embedding, and ontology development
on knowledge bases.
In contrast, virtual knowledge graphs do not store information in specialized databases.
They rely on an underlying relational database or data lake to answer queries on the
graph. Such a virtual knowledge graph system must be properly configured in order to
answer the queries correctly. This specific configuration is done through a set of
mappings that define the relationship between the elements of the data source and the
structure and ontology of the virtual knowledge graph.


== Using a knowledge graph for reasoning over data ==

A knowledge graph formally represents semantics by describing entities and their
relationships. Knowledge graphs may make use of ontologies as a schema layer. By doing
this, they allow logical inference for retrieving implicit knowledge rather than only
allowing queries requesting explicit knowledge.
In order to allow the use of knowledge graphs in various machine learning tasks, several
methods for deriving latent feature representations of entities and relations have been
devised. These knowledge graph embeddings allow them to be connected to machine learning
methods that require feature vectors like word embeddings. This can complement other
estimates of conceptual similarity.
Models for generating useful knowledge graph embeddings are commonly the domain of graph
neural networks (GNNs). GNNs are deep learning architectures that comprise edges and
nodes, which correspond well to the entities and relationships of knowledge graphs. The
topology and data structures afforded by GNNs provide a convenient domain for
semi-supervised learning, wherein the network is trained to predict the value of a node
embedding (provided a group of adjacent nodes and their edges) or edge (provided a pair
of nodes). These tasks serve as fundamental abstractions for more complex tasks such as
knowledge graph reasoning and alignment.


=== Entity alignment ===

As new knowledge graphs are produced across a variety of fields and contexts, the same
entity will inevitably be represented in multiple graphs. However, because no single
standard for the construction or representation of knowledge graph exists, resolving
which entities from disparate graphs correspond to the same real world subject is a
non-trivial task. This task is known as knowledge graph entity alignment, and is an
active area of research.
Strategies for entity alignment generally seek to identify similar substructures,
semantic relationships, shared attributes, or combinations of all three between two
distinct knowledge graphs. Entity alignment methods use these structural similarities
between generally non-isomorphic graphs to predict which nodes corresponds to the same
entity.
The recent successes of large language models (LLMs), in particular their effectiveness
at producing syntactically meaningful embeddings, has spurred the use of LLMs in the
task of entity alignment.
As the amount of data stored in knowledge graphs grows, developing dependable methods
for knowledge graph entity alignment becomes an increasingly crucial step in the
integration and cohesion of knowledge graph data.


== See also ==
Concept map – Diagram showing relationships among concepts
Formal semantics (natural language) – Formal study of linguistic meaning
Graph database – Database using graph structures for queries
Knowledge base – Information repository with multiple applications
Knowledge graph embedding – Dimensionality reduction of graph-based semantic data
objects [machine learning task]
Logical graph – Type of diagrammatic notation for propositional logicPages displaying
short descriptions of redirect targets
Semantic integration – Interrelating info from diverse sources
Semantic technology – Technology to help machines understand data
Topic map – Knowledge organization system
Vadalog – Type of Knowledge Graph Management System
Wikibase- Mediawiki Software extensions for creating knowledge bases
Wikidata - Free Knowledge Database Project
YAGO (database) – Open-source information repository


== References ==


== External links ==

Will Douglas Heaven (4 September 2020). "This know-it-all AI learns by reading the
entire web nonstop". MIT Technology Review. Retrieved 5 September 2020. Diffbot is
building the biggest-ever knowledge graph by applying image recognition and
natural-language processing to billions of web pages.

## Related context: Linked data
Related source: https://en.wikipedia.org/wiki/Linked_data

In computing, linked data is structured data which is associated with ("linked" to)
other data. Interlinking makes the data more useful through semantic queries.
Tim Berners-Lee, director of the World Wide Web Consortium (W3C), coined the term in a
2006 design note about the Semantic Web project.
Part of the vision of linked data is for the Internet to become a global database.
Linked data builds upon standard Web technologies such as HTTP, RDF and URIs, but rather
than using them to serve web pages and hyperlinks only for human readers, it extends
them to share information in a way that can be read automatically by computers (machine
readable).
Linked data may also be open data, in which case it is usually described as Linked Open
Data.


== Principles ==
In his 2006 "Linked Data" note, Tim Berners-Lee outlined four principles of linked data,
paraphrased along the following lines:

Uniform Resource Identifiers (URIs) should be used to name and identify individual
things.
HTTP URIs should be used to allow these things to be looked up, interpreted, and
subsequently "dereferenced".
Useful information about what a name identifies should be provided through open
standards such as RDF, SPARQL, etc.
When publishing data on the Web, other things should be referred to using their HTTP
URI-based names.
Tim Berners-Lee later restated these principles at a 2009 TED conference, again
paraphrased along the following lines:

All conceptual things should have a name starting with HTTP.
Looking up an HTTP name should return useful data about the thing in question in a
standard format.
Anything else that that same thing has a relationship with through its data should also
be given a name beginning with HTTP.


== Components ==
Thus, we can identify the following components as essential to a global Linked Data
system as envisioned, and to any actual Linked Data subset within it:

URIs
HTTP
Structured data using controlled vocabulary terms and dataset definitions expressed in
Resource Description Framework serialization formats such as RDFa, RDF/XML, N3, Turtle,
or JSON-LD
Linked Data Platform
CSV-W


== Linked open data ==
Linked open data are linked data that are open data. Tim Berners-Lee gives the clearest
definition of linked open data as differentiated from linked data.

Linked Open Data (LOD) is Linked Data which is released under an open license, which
does not impede its reuse for free.
Large linked open data sets include DBpedia, Wikibase, Wikidata and Open ICEcat.


=== 5-star linked open data ===

