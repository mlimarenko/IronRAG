# Information retrieval

Source: https://en.wikipedia.org/wiki/Information_retrieval
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

Information retrieval (IR) in computing and information science is the task of
identifying and retrieving information system resources that are relevant to an
information need. The information need can be specified in the form of a search query.
In the case of document retrieval, queries can be based on full-text or other
content-based indexing. Information retrieval is the science of searching for
information in a document, searching for documents themselves, and also searching for
the metadata that describes data, and for databases of texts, images, or sounds.
Cross-modal retrieval implies retrieval across modalities.
Automated information retrieval systems are used to reduce what has been called
information overload. An IR system is a software system that provides access to books,
journals, and other documents, as well as storing and managing those documents. Web
search engines are the most visible IR applications.


== Overview ==
An information retrieval process begins when a user enters a query into the system.
Queries are formal statements of information needs, for example search strings in web
search engines. In information retrieval, a query does not uniquely identify a single
object in the collection. Instead, several objects may match the query, perhaps with
different degrees of relevance.
An object is an entity that is represented by information in a content collection or
database. User queries are matched against the database information. However, as opposed
to classical SQL queries of a database, in information retrieval the results returned
may or may not match the query, so results are typically ranked. This ranking of results
is a key difference of information retrieval searching compared to database searching.
Depending on the application the data objects may be, for example, text documents,
images, audio, mind maps or videos. Often the documents themselves are not kept or
stored directly in the IR system, but are instead represented in the system by document
surrogates or metadata.
Most IR systems compute a numeric score on how well each object in the database matches
the query, and rank the objects according to this value. The top ranking objects are
then shown to the user. The process may then be iterated if the user wishes to refine
the query.


== History ==

The idea of using computers to search for relevant pieces of information was popularized
in the article As We May Think by Vannevar Bush in 1945. It would appear that Bush was
inspired by patents for a 'statistical machine' – filed by Emanuel Goldberg in the 1920s
and 1930s – that searched for documents stored on film. The first description of a
computer searching for information was described by Holmstrom in 1948, detailing an
early mention of the Univac computer. Automated information retrieval systems were
introduced in the 1950s: one even featured in the 1957 romantic comedy Desk Set. In the
1960s, the first large information retrieval research group was formed by Gerard Salton
at Cornell. By the 1970s several different retrieval techniques had been shown to
perform well on small text corpora such as the Cranfield collection (several thousand
documents). Large-scale retrieval systems, such as the Lockheed Dialog system, came into
use early in the 1970s.
In 1992, the US Department of Defense along with the National Institute of Standards and
Technology (NIST), cosponsored the Text Retrieval Conference (TREC) as part of the
TIPSTER text program. The aim of this was to look into the information retrieval
community by supplying the infrastructure that was needed for evaluation of text
retrieval methodologies on a very large text collection. This catalyzed research on
methods that scale to huge corpora. The introduction of web search engines has boosted
the need for very large scale retrieval systems even further.
By the late 1990s, the rise of the World Wide Web fundamentally transformed information
retrieval. While early search engines such as AltaVista (1995) and Yahoo! (1994) offered
keyword-based retrieval, they were limited in scale and ranking refinement. The
breakthrough came in 1998 with the founding of Google, which introduced the PageRank
algorithm, using the web's hyperlink structure to assess page importance and improve
relevance ranking.
During the 2000s, web search systems evolved rapidly with the integration of machine
learning techniques. These systems began to incorporate user behavior data (e.g.,
click-through logs), query reformulation, and content-based signals to improve search
accuracy and personalization. In 2009, Microsoft launched Bing, introducing features
that would later incorporate semantic web technologies through the development of its
Satori knowledge base. Academic analysis have highlighted Bing's semantic capabilities,
including structured data use and entity recognition, as part of a broader industry
shift toward improving search relevance and understanding user intent through natural
language processing.
A major leap occurred in 2018, when Google deployed BERT (Bidirectional Encoder
Representations from Transformers) to better understand the contextual meaning of
queries and documents. This marked one of the first times deep neural language models
were used at scale in real-world retrieval systems. BERT's bidirectional training
enabled a more refined comprehension of word relationships in context, improving the
handling of natural language queries. Because of its success, transformer-based models
gained traction in academic research and commercial search applications.
Simultaneously, the research community began exploring neural ranking models that
outperformed traditional lexical-based methods. Long-standing benchmarks such as the
Text REtrieval Conference (TREC), initiated in 1992, and more recent evaluation
frameworks Microsoft MARCO(MAchine Reading COmprehension) (2019) became central to
training and evaluating retrieval systems across multiple tasks and domains. MS MARCO
has also been adopted in the TREC Deep Learning Tracks, where it serves as a core
dataset for evaluating advances in neural ranking models within a standardized
benchmarking environment.
As deep learning became integral to information retrieval systems, researchers began to
categorize neural approaches into three broad classes: sparse, dense, and hybrid models.
Sparse models, including traditional term-based methods and learned variants like
SPLADE, rely on interpretable representations and inverted indexes to enable efficient
exact term matching with added semantic signals. Dense models, such as dual-encoder
architectures like ColBERT, use continuous vector embeddings to support semantic
similarity beyond keyword overlap. Hybrid models aim to combine the advantages of both,
balancing the lexical (token) precision of sparse methods with the semantic depth of
dense models. This way of categorizing models balances scalability, relevance, and
efficiency in retrieval systems.
As IR systems increasingly rely on deep learning, concerns around bias, fairness, and
explainability have also come to the picture. Research is now focused not just on
relevance and efficiency, but on transparency, accountability, and user trust in
retrieval algorithms.


== Applications ==
Areas where information retrieval techniques are employed include (the entries are in
alphabetical order within each category):


=== General applications ===
Digital libraries
Information filtering
Recommender systems
Media search
Blog search
Image retrieval
3D retrieval
Music retrieval
News search
Speech retrieval
Video retrieval
Search engines
Site search
Desktop search
Enterprise search
Federated search
Mobile search
Social search
Web search


=== Domain-specific applications ===
Expert search finding
Genomic information retrieval
Geographic information retrieval
Information retrieval for chemical structures
Information retrieval in software engineering
Legal information retrieval
Vertical search


=== Other retrieval methods ===
Methods/Techniques in which information retrieval techniques are employed include:

Cross-modal retrieval
Adversarial information retrieval
Automatic summarization
Multi-document summarization
Compound term processing
Cross-lingual retrieval
Document classification
Spam filtering
Question answering


== Model types ==

In order to effectively retrieve relevant documents by IR strategies, the documents are
typically transformed into a suitable representation. Each retrieval strategy
incorporates a specific model for its document representation purposes. The picture on
the right illustrates the relationship of some common models. In the picture, the models
are categorized according to two dimensions: the mathematical basis and the properties
of the model.


=== First dimension: mathematical basis ===
Set-theoretic models represent documents as sets of words or phrases. Similarities are
usually derived from set-theoretic operations on those sets. Common models are:
Standard Boolean model
Extended Boolean model
Fuzzy retrieval
Algebraic models represent documents and queries usually as vectors, matrices, or
tuples. The similarity of the query vector and document vector is represented as a
scalar value.
Vector space model
Generalized vector space model
(Enhanced) Topic-based Vector Space Model
Extended Boolean model
Latent semantic indexing a.k.a. latent semantic analysis
Probabilistic models treat the process of document retrieval as a probabilistic
inference. Similarities are computed as probabilities that a document is relevant for a
given query. Probabilistic theorems like Bayes' theorem are often used in these models.
Binary Independence Model
Probabilistic relevance model on which is based the okapi (BM25) relevance function
Uncertain inference
Language models
Divergence-from-randomness model
Latent Dirichlet allocation
Feature-based retrieval models view documents as vectors of values of feature functions
(or just features) and seek the best way to combine these features into a single
relevance score, typically by learning to rank methods. Feature functions are arbitrary
functions of document and query, and as such can easily incorporate almost any other
retrieval model as just another feature.
Data fusion models: Data fusion in information retrieval combines results from multiple
search systems or retrieval models to improve performance. By merging ranked lists, it
leverages the strengths of diverse approaches, often enhancing recall and precision.
Common methods include score normalization and voting techniques like CombSUM or Borda
count. This meta-search strategy is particularly effective when individual systems have
complementary coverage or when query difficulty varies, producing a more robust and
reliable final ranking sparsity.


=== Second dimension: properties of the model ===
Models without term-interdependencies treat different terms/words as independent. This
fact is usually represented in vector space models by the orthogonality assumption of
term vectors or in probabilistic models by an independency assumption for term
variables.
Models with immanent term interdependencies allow a representation of interdependencies
between terms. However the degree of the interdependency between two terms is defined by
the model itself. It is usually directly or indirectly derived (e.g. by dimensional
reduction) from the co-occurrence of those terms in the whole set of documents.
Models with transcendent term interdependencies allow a representation of
interdependencies between terms, but they do not allege how the interdependency between
two terms is defined. They rely on an external source for the degree of interdependency
between two terms. (For example, a human or sophisticated algorithms.)


=== Third Dimension: representational approach-based classification ===
In addition to the theoretical distinctions, modern information retrieval models are
also categorized on how queries and documents are represented and compared, using a
practical classification distinguishing between sparse, dense and hybrid models.

Sparse models utilize interpretable, term-based representations and typically rely on
inverted index structures. Classical methods such as TF-IDF and BM25 fall under this
category, along with more recent learned sparse models that integrate neural
architectures while retaining sparsity.
Dense models represent queries and documents as continuous vectors using deep learning
models, typically transformer-based encoders. These models enable semantic similarity
matching beyond exact term overlap and are used in tasks involving semantic search and
question answering.
Hybrid models aim to combine the strengths of both approaches, integrating lexical
(tokens) and semantic signals through score fusion, late interaction, or multi-stage
