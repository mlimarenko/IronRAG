# Vector database

Source: https://en.wikipedia.org/wiki/Vector_database
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

A vector database, vector store or vector search engine is a database that stores and
retrieves embeddings of data in vector space. Vector databases typically implement
approximate nearest neighbor algorithms so users can search for records semantically
similar to a given input, unlike traditional databases which primarily look up records
by exact match. Use-cases for vector databases include similarity search, semantic
search, multi-modal search, recommendations engines, object detection, and
retrieval-augmented generation (RAG).
Vector embeddings are mathematical representations of data in a high-dimensional space.
In this space, each dimension corresponds to a feature of the data, with the number of
dimensions ranging from a few hundred to tens of thousands, depending on the complexity
of the data being represented. Each data item is represented by one vector in this
space. Words, phrases, or entire documents, as well as images, audio, and other types of
data, can all be vectorized.
These feature vectors may be computed from the raw data using machine learning methods
such as feature extraction algorithms, word embeddings or deep learning networks. The
goal is that semantically similar data items receive feature vectors close to each
other.


== Techniques ==
The most important techniques for similarity search on high-dimensional vectors include:

Hierarchical Navigable Small World (HNSW) graphs
Locality-sensitive Hashing (LSH) and Sketching
Product Quantization (PQ)
Inverted Files
and combinations of these techniques.
In recent benchmarks, HNSW-based implementations have been among the best performers.
Conferences such as the International Conference on Similarity Search and Applications
(SISAP) and the Conference on Neural Information Processing Systems (NeurIPS) have
hosted competitions on vector search in large databases.


== Applications ==
Vector databases are used in a wide range of machine learning applications including
similarity search, semantic search, multi-modal search, recommendations engines, object
detection, and retrieval-augmented generation.


=== Retrieval-augmented generation ===

An especially common use-case for vector databases is in retrieval-augmented generation
(RAG), a method to improve domain-specific responses of large language models. The
retrieval component of a RAG can be any search system, but is most often implemented as
a vector database. Text documents describing the domain of interest are collected, and
for each document or document section, a feature vector (known as an "embedding") is
computed, typically using a deep learning network, and stored in a vector database along
with a link to the document. Given a user prompt, the feature vector of the prompt is
computed, and the database is queried to retrieve the most relevant documents. These are
then automatically added into the context window of the large language model, and the
large language model proceeds to create a response to the prompt given this context.


== Implementations ==


== See also ==
Curse of dimensionality – Difficulties arising when analyzing data with many aspects
("dimensions")
Graph database – Database using graph structures for queries
Machine learning – Study of algorithms that improve automatically through experience
Nearest neighbor search – Optimization problem in computer science
Recommender system – System to predict users' preferences


== References ==


== External links ==
Sawers, Paul (2024-04-20). "Why vector databases are having a moment as the AI hype
cycle peaks". TechCrunch. Retrieved 2024-04-23.

## Related context: Approximate nearest neighbor search
Related source: https://en.wikipedia.org/wiki/Approximate_nearest_neighbor_search

Nearest neighbor search (NNS), as a form of proximity search, is the optimization
problem of finding the point in a given set that is closest (or most similar) to a given
point. Closeness is typically expressed in terms of a dissimilarity function: the less
similar the objects, the larger the function values.
Formally, the nearest neighbor (NN) search problem is defined as follows: given a set S
of points in a space M and a query point



q
∈
M


{\displaystyle q\in M}

, find the closest point in S to q. Donald Knuth in volume 3 of The Art of Computer
Programming (1973) called it the post-office problem, referring to an application of
assigning to a residence the nearest post office. A direct generalization of this
problem is a k-NN search, where we need to find the k closest points.
Most commonly M is a metric space and dissimilarity is expressed as a distance metric,
which is symmetric and satisfies the triangle inequality. Even more common, M is taken
to be the d-dimensional vector space where dissimilarity is measured using the Euclidean
distance, Manhattan distance or other distance metric. However, the dissimilarity
function can be arbitrary. One example is asymmetric Bregman divergence, for which the
triangle inequality does not hold.


== Applications ==
The nearest neighbor search problem arises in numerous fields of application, including:

Pattern recognition – in particular for optical character recognition
Statistical classification – see k-nearest neighbor algorithm
Computer vision – for point cloud registration
Computational geometry – see Closest pair of points problem
Cryptanalysis – for lattice problem
Databases – e.g. content-based image retrieval
Coding theory – see maximum likelihood decoding
Semantic search
Data compression – see MPEG-2 standard
Robotic sensing
Recommendation systems, e.g. see Collaborative filtering
Internet marketing – see contextual advertising and behavioral targeting
DNA sequencing
Spell checking – suggesting correct spelling
Plagiarism detection
Similarity scores for predicting career paths of professional athletes.
Cluster analysis – assignment of a set of observations into subsets (called clusters) so
that observations in the same cluster are similar in some sense, usually based on
Euclidean distance
Chemical similarity
Sampling-based motion planning


== Methods ==
Various solutions to the NNS problem have been proposed. The quality and usefulness of
the algorithms are determined by the time complexity of queries as well as the space
complexity of any search data structures that must be maintained. The informal
observation usually referred to as the curse of dimensionality states that there is no
general-purpose exact solution for NNS in high-dimensional Euclidean space using
polynomial preprocessing and polylogarithmic search time.


=== Exact methods ===


==== Linear search ====
The simplest solution to the NNS problem is to compute the distance from the query point
to every other point in the database, keeping track of the "best so far". This
algorithm, sometimes referred to as the naive approach, has a running time of O(dN),
where N is the cardinality of S and d is the dimensionality of S. There are no search
data structures to maintain, so the linear search has no space complexity beyond the
storage of the database. Naive search can, on average, outperform space partitioning
approaches on higher dimensional spaces.
The absolute distance is not required for distance comparison, only the relative
distance. In geometric coordinate systems the distance calculation can be sped up
considerably by omitting the square root calculation from the distance calculation
between two coordinates. The distance comparison will still yield identical results.


==== Space partitioning ====
Since the 1970s, the branch and bound methodology has been applied to the problem. In
the case of Euclidean space, this approach encompasses spatial index or spatial access
methods. Several space-partitioning methods have been developed for solving the NNS
problem. Perhaps the simplest is the k-d tree, which iteratively bisects the search
space into two regions containing half of the points of the parent region. Queries are
performed via traversal of the tree from the root to a leaf by evaluating the query
point at each split. Depending on the distance specified in the query, neighboring
branches that might contain hits may also need to be evaluated. For constant dimension
query time, average complexity is O(log N) in the case of randomly distributed points,
worst case complexity is O(kN^(1-1/k))
Alternatively the R-tree data structure was designed to support nearest neighbor search
in dynamic context, as it has efficient algorithms for insertions and deletions such as
the R* tree. R-trees can yield nearest neighbors not only for Euclidean distance, but
can also be used with other distances.
In the case of general metric space, the branch-and-bound approach is known as the
metric tree approach. Particular examples include vp-tree and BK-tree methods.

Using a set of points taken from a 3-dimensional space and put into a BSP tree, and
given a query point taken from the same space, a possible solution to the problem of
finding the nearest point-cloud point to the query point is given in the following
description of an algorithm.
(Strictly speaking, no such point may exist, because it may not be unique. But in
practice, usually we only care about finding any one of the subset of all point-cloud
points that exist at the shortest distance to a given query point.) The idea is, for
each branching of the tree, guess that the closest point in the cloud resides in the
half-space containing the query point. This may not be the case, but it is a good
heuristic. After having recursively gone through all the trouble of solving the problem
for the guessed half-space, now compare the distance returned by this result with the
shortest distance from the query point to the partitioning plane. This latter distance
is that between the query point and the closest possible point that could exist in the
half-space not searched. If this distance is greater than that returned in the earlier
result, then clearly there is no need to search the other half-space. If there is such a
need, then you must go through the trouble of solving the problem for the other half
space, and then compare its result to the former result, and then return the proper
result. The performance of this algorithm is nearer to logarithmic time than linear time
when the query point is near the cloud, because as the distance between the query point
and the closest point-cloud point nears zero, the algorithm needs only perform a look-up
using the query point as a key to get the correct result.


=== Approximation methods ===
An approximate nearest neighbor search algorithm is allowed to return points whose
distance from the query is at most



c


{\displaystyle c}

times the distance from the query to its nearest points. The appeal of this approach is
that, in many cases, an approximate nearest neighbor is almost as good as the exact one.
In particular, if the distance measure accurately captures the notion of user quality,
then small differences in the distance should not matter.


==== Greedy search in proximity neighborhood graphs ====
Proximity graph methods (such as navigable small world graphs and HNSW) are considered
the current state-of-the-art for the approximate nearest neighbors search.
The methods are based on greedy traversing in proximity neighborhood graphs



G
(
V
,
E
)


{\displaystyle G(V,E)}

