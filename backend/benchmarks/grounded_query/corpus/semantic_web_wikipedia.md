# Semantic Web

Source: https://en.wikipedia.org/wiki/Semantic_Web
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

The Semantic Web, sometimes known as Web 3.0, is an extension of the World Wide Web
through standards set by the World Wide Web Consortium (W3C). The goal of the Semantic
Web is to make Internet data machine-readable.
To enable the encoding of semantics with the data, technologies such as Resource
Description Framework (RDF) and Web Ontology Language (OWL) are used. These technologies
are used to formally represent metadata. For example, ontology can describe concepts,
relationships between entities, and categories of things. These embedded semantics offer
significant advantages such as reasoning over data and operating with heterogeneous data
sources.
These standards promote common data formats and exchange protocols on the Web,
fundamentally the RDF. According to the W3C, "The Semantic Web provides a common
framework that allows data to be shared and reused across application, enterprise, and
community boundaries." The Semantic Web is therefore regarded as an integrator across
different content and information applications and systems.


== History ==
The term was coined by Tim Berners-Lee for a web of data (or data web) that can be
processed by machines—that is, one in which much of the meaning is machine-readable.
While its critics have questioned its feasibility, proponents argue that applications in
library and information science, industry, biology and human sciences research have
already proven the validity of the original concept.
The idea of adding semantics to the Web predates the term itself. Berners-Lee discussed
the need for semantics in the Web at the first International World Wide Web Conference
in 1994. In 1998, he published a design document titled "Semantic Web Road map",
outlining the architecture for a web of machine-processable data. The first patent for
the creation of a semantic web was filed by Amit Sheth et al. on 30 October 2001.
Berners-Lee originally expressed his vision of the Semantic Web in 1999 as follows:

I have a dream for the Web [in which computers] become capable of analyzing all the data
on the Web – the content, links, and transactions between people and computers. A
"Semantic Web", which makes this possible, has yet to emerge, but when it does, the
day-to-day mechanisms of trade, bureaucracy and our daily lives will be handled by
machines talking to machines. The "intelligent agents" people have touted for ages will
finally materialize.
The 2001 Scientific American article by Berners-Lee, Hendler, and Lassila described an
expected evolution of the existing Web to a Semantic Web. In 2006, Berners-Lee and
colleagues stated that: "This simple idea…remains largely unrealized".
In 2013, more than four million Web domains (out of roughly 250 million total) contained
Semantic Web markup.


== Example ==
In the following example, the text "Paul Schuster was born in Dresden" on a website will
be annotated, connecting a person with their place of birth. The following HTML fragment
shows how a small graph is being described, in RDFa-syntax using a schema.org vocabulary
and a Wikidata ID:

The example defines the following five triples (shown in Turtle syntax). Each triple
represents one edge in the resulting graph: the first element of the triple (the
subject) is the name of the node where the edge starts, the second element (the
predicate) the type of the edge, and the last and third element (the object) either the
name of the node where the edge ends or a literal value (e.g. a text, a number, etc.).

The triples result in the graph shown in the given figure.

One of the advantages of using Uniform Resource Identifiers (URIs) is that they can be
dereferenced using the HTTP protocol. According to the so-called Linked Open Data
principles, such a dereferenced URI should result in a document that offers further data
about the given URI. In this example, all URIs, both for edges and nodes (e.g.
http://schema.org/Person, http://schema.org/birthPlace,
http://www.wikidata.org/entity/Q1731) can be dereferenced and will result in further RDF
graphs, describing the URI, e.g. that Dresden is a city in Germany, or that a person, in
the sense of that URI, can be fictional.
The second graph shows the previous example, but now enriched with a few of the triples
from the documents that result from dereferencing https://schema.org/Person (green edge)
and https://www.wikidata.org/entity/Q1731 (blue edges).
Additionally to the edges given in the involved documents explicitly, edges can be
automatically inferred: the triple

from the original RDFa fragment and the triple

from the document at https://schema.org/Person (green edge in the figure) allow to infer
the following triple, given OWL semantics (red dashed line in the second Figure):


== Background ==

The concept of the semantic network model was formed in the early 1960s by researchers
such as the cognitive scientist Allan M. Collins, linguist Ross Quillian and
psychologist Elizabeth F. Loftus as a form to represent semantically structured
knowledge. When applied in the context of the modern internet, it extends the network of
hyperlinked human-readable web pages by inserting machine-readable metadata about pages
and how they are related to each other.  This enables automated agents to access the Web
more intelligently and perform more tasks on behalf of users. The term "Semantic Web"
was coined by Tim Berners-Lee, the inventor of the World Wide Web and director of the
World Wide Web Consortium ("W3C"), which oversees the development of proposed Semantic
Web standards. He defines the Semantic Web as "a web of data that can be processed
directly and indirectly by machines".
Many of the technologies proposed by the W3C already existed before they were positioned
under the W3C umbrella. These are used in various contexts, particularly those dealing
with information that encompasses a limited and defined domain, and where sharing data
is a common necessity, such as scientific research or data exchange among businesses. In
addition, other technologies with similar goals have emerged, such as microformats.


=== Limitations of HTML ===
Many files on a typical computer can be loosely divided into either human-readable
documents, or machine-readable data. Examples of human-readable document files are mail
messages, reports, and brochures. Examples of machine-readable data files are calendars,
address books, playlists, and spreadsheets, which are presented to a user using an
application program that lets the files be viewed, searched, and combined.
Currently, the World Wide Web is based mainly on documents written in Hypertext Markup
Language (HTML), a markup convention that is used for coding a body of text interspersed
with multimedia objects such as images and interactive forms. Metadata tags provide a
method by which computers can categorize the content of web pages. In the examples
below, the field names "keywords", "description" and "author" are assigned values such
as "computing", and "cheap widgets for sale" and "John Doe".

Because of this metadata tagging and categorization, other computer systems that want to
access and share this data can easily identify the relevant values.
With HTML and a tool to render it (perhaps web browser software, perhaps another user
agent), one can create and present a page that lists items for sale. The HTML of this
catalog page can make simple, document-level assertions such as "this document's title
is 'Widget Superstore'", but there is no capability within the HTML itself to assert
unambiguously that, for example, item number X586172 is an Acme Gizmo with a retail
price of €199, or that it is a consumer product. Rather, HTML can only say that the span
of text "X586172" is something that should be positioned near "Acme Gizmo" and "€199",
etc. There is no way to say "this is a catalog" or even to establish that "Acme Gizmo"
is a kind of title or that "€199" is a price. There is also no way to express that these
pieces of information are bound together in describing a discrete item, distinct from
other items perhaps listed on the page.
Semantic HTML refers to the traditional HTML practice of markup following intention,
rather than specifying layout details directly. For example, the use of <em> denoting
"emphasis" rather than <i>, which specifies italics. Layout details are left up to the
browser, in combination with Cascading Style Sheets. But this practice falls short of
specifying the semantics of objects such as items for sale or prices.
Microformats extend HTML syntax to create machine-readable semantic markup about objects
including people, organizations, events and products. Similar initiatives include RDFa,
Microdata and Schema.org.


=== Semantic Web solutions ===
The Semantic Web takes the solution further. It involves publishing in languages
specifically designed for data: Resource Description Framework (RDF), Web Ontology
Language (OWL), and Extensible Markup Language (XML). HTML describes documents and the
links between them. RDF, OWL, and XML, by contrast, can describe arbitrary things such
as people, meetings, or airplane parts.
These technologies are combined in order to provide descriptions that supplement or
replace the content of Web documents. Thus, content may manifest itself as descriptive
data stored in Web-accessible databases, or as markup within documents (particularly, in
Extensible HTML (XHTML) interspersed with XML, or, more often, purely in XML, with
layout or rendering cues stored separately). The machine-readable descriptions enable
content managers to add meaning to the content, i.e., to describe the structure of the
knowledge we have about that content. In this way, a machine can process knowledge
itself, instead of text, using processes similar to human deductive reasoning and
inference, thereby obtaining more meaningful results and helping computers to perform
automated information gathering and research.
An example of a tag that would be used in a non-semantic web page:

Encoding similar information in a semantic web page might look like this:

Tim Berners-Lee calls the resulting network of Linked Data the Giant Global Graph, in
contrast to the HTML-based World Wide Web. Berners-Lee posits that if the past was
document sharing, the future is data sharing. His answer to the question of "how"
provides three points of instruction. One, a URL should point to the data. Two, anyone
accessing the URL should get data back. Three, relationships in the data should point to
additional URLs with data.


==== Tags and identifiers ====
Tags, including hierarchical categories and tags that are collaboratively added and
maintained (e.g. with folksonomies) can be considered part of, of potential use to or a
step towards the semantic Web vision.
Unique identifiers, including hierarchical categories and collaboratively added ones,
analysis tools and metadata, including tags, can be used to create forms of semantic
webs – webs that are to a certain degree semantic.  In particular, such has been used
for structuring scientific research i.a. by research topics and scientific fields by the
projects OpenAlex, Wikidata and Scholia which are under development and provide APIs,
Web-pages, feeds and graphs for various semantic queries.


=== Web 3.0 ===
Tim Berners-Lee has described the Semantic Web as a component of Web 3.0.

People keep asking what Web 3.0 is. I think maybe when you've got an overlay of scalable
vector graphics – everything rippling and folding and looking misty – on Web 2.0 and
access to a semantic Web integrated across a huge space of data, you'll have access to
an unbelievable data resource …
"Semantic Web" is sometimes used as a synonym for "Web 3.0", though the definition of
each term varies.


=== Beyond Web 3.0 ===
The next generation of the Web is often termed Web 4.0, but its definition is not clear.
According to some sources, it is a Web that involves artificial intelligence, the
internet of things, pervasive computing, ubiquitous computing and the Web of Things
among other concepts. According to the European Union, Web 4.0 is "the expected fourth
generation of the World Wide Web. Using advanced artificial and ambient intelligence,
the internet of things, trusted blockchain transactions, virtual worlds and XR
capabilities, digital and real objects and environments are fully integrated and
communicate with each other, enabling truly intuitive, immersive experiences, seamlessly
blending the physical and digital worlds".


== Challenges ==
Some of the challenges for the Semantic Web include vastness, vagueness, uncertainty,
inconsistency, and deceit. Automated reasoning systems will have to deal with all of
these issues in order to deliver on the promise of the Semantic Web.

Vastness: The World Wide Web contains many billions of pages. The SNOMED CT medical
terminology ontology alone contains 370,000 class names, and existing technology has not
yet been able to eliminate all semantically duplicated terms. Any automated reasoning
system will have to deal with truly huge inputs.
Vagueness: These are imprecise concepts like "young" or "tall". This arises from the
vagueness of user queries, of concepts represented by content providers, of matching
query terms to provider terms and of trying to combine different knowledge bases with
overlapping but subtly different concepts. Fuzzy logic is the most common technique for
dealing with vagueness.
Uncertainty: These are precise concepts with uncertain values. For example, a patient
might present a set of symptoms that correspond to a number of different distinct
diagnoses each with a different probability. Probabilistic reasoning techniques are
generally employed to address uncertainty.
Inconsistency: These are logical contradictions that will inevitably arise during the
development of large ontologies, and when ontologies from separate sources are combined.
Deductive reasoning fails catastrophically when faced with inconsistency, because
"anything follows from a contradiction". Defeasible reasoning and paraconsistent
reasoning are two techniques that can be employed to deal with inconsistency.
Deceit: This is when the producer of the information is intentionally misleading the
consumer of the information. Cryptography techniques are currently utilized to alleviate
this threat. By providing a means to determine the information's integrity, including
that which relates to the identity of the entity that produced or published the
information, however credibility issues still have to be addressed in cases of potential
deceit.
This list of challenges is illustrative rather than exhaustive, and it focuses on the
challenges to the "unifying logic" and "proof" layers of the Semantic Web. The World
Wide Web Consortium (W3C) Incubator Group for Uncertainty Reasoning for the World Wide
Web (URW3-XG) final report lumps these problems together under the single heading of
"uncertainty". Many of the techniques mentioned here will require extensions to the Web
Ontology Language (OWL) for example to annotate conditional probabilities. This is an
