# Named-entity recognition

Source: https://en.wikipedia.org/wiki/Named-entity_recognition
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

Named-entity recognition (NER) (also known as (named) entity identification, entity
chunking, and entity extraction) is a subtask of information extraction that seeks to
locate and classify named entities mentioned in unstructured text into pre-defined
categories such as person names (PER), organizations (ORG), locations (LOC),
geopolitical entities (GPE), vehicles (VEH), medical codes, time expressions,
quantities, monetary values, percentages, etc.
Most research on NER/NEE systems has been structured as taking an unannotated block of
text, such as transducing:

Jim bought 300 shares of Acme Corp. in 2006.
into an annotated block of text that highlights the names of entities:

[Jim]Person bought 300 shares of [Acme Corp.]Organization in [2006]Time.
In this example, a person name consisting of one token, a two-token company name and a
temporal expression have been detected and classified.


== Problem ==


=== Definition ===
In the expression named entity, the word named restricts the task to those entities for
which one or many strings, such as words or phrases, stand (fairly) consistently for
some referent. This is closely related to rigid designators, as defined by Saul Kripke,
although in practice NER deals with many names and referents that are not
philosophically "rigid". For instance, the automotive company created by Henry Ford in
1903 can be referred to as Ford or Ford Motor Company, although "Ford" can refer to many
other entities as well (see Ford). Rigid designators include proper names as well as
terms for certain biological species and substances, but exclude pronouns (such as "it";
see coreference resolution), descriptions that pick out a referent by its properties
(see also De dicto and de re), and names for kinds of things as opposed to individuals
(for example "Bank").
Full named-entity recognition is often broken down, conceptually and possibly also in
implementations, as two distinct problems: detection of names, and classification of the
names by the type of entity they refer to (e.g. person, organization, or location).
The first phase is typically simplified to a segmentation problem: names are defined to
be contiguous spans of tokens, with no nesting, so that "Bank of America" is a single
name, disregarding the fact that inside this name, the substring "America" is itself a
name. This segmentation problem is formally similar to chunking. The second phase
requires choosing an ontology by which to organize categories of things.
Temporal expressions and some numerical expressions (e.g., money, percentages, etc.) may
also be considered as named entities in the context of the NER task. While some
instances of these types are good examples of rigid designators (e.g., the year 2001)
there are also many invalid ones (e.g., I take my vacations in “June”). In the first
case, the year 2001 refers to the 2001st year of the Gregorian calendar. In the second
case, the month June may refer to the month of an undefined year (past June, next June,
every June, etc.). It is arguable that the definition of named entity is loosened in
such cases for practical reasons. The definition of the term named entity is therefore
not strict and often has to be explained in the context in which it is used.
Certain hierarchies of named entity types have been proposed in the literature. BBN
categories, proposed in 2002, are used for question answering and consists of 29 types
and 64 subtypes. Sekine's extended hierarchy, proposed in 2002, is made of 200 subtypes.
More recently, in 2011 Ritter used a hierarchy based on common Freebase entity types in
ground-breaking experiments on NER over social media text.


=== Difficulties ===
NER can have reference resolution ambiguities where the same name can refer to different
entities of the same type. For example, "JFK" can refer to the former president or his
son.
The same name can refer to completely different types. "JFK" might refer to the airport
in New York. "IRA" can refer to Individual Retirement Account or International Reading
Association.
This can be caused by metonymy. For example, "The White House" can refer to an
organization instead of a location.


=== Formal evaluation ===
To evaluate the quality of an NER system's output, several measures have been defined.
The usual measures are called precision, recall, and F1 score. However, several issues
remain in just how to calculate those values.
These statistical measures work reasonably well for the obvious cases of finding or
missing a real entity exactly; and for finding a non-entity. However, NER can fail in
many other ways, many of which are arguably "partially correct", and should not be
counted as complete success or failures. For example, identifying a real entity, but:

with fewer tokens than desired (for example, missing the last token of "John Smith,
M.D.")
with more tokens than desired (for example, including the first word of "The University
of MD")
partitioning adjacent entities differently (for example, treating "Smith, Jones
Robinson" as 2 vs. 3 entities)
assigning it a completely wrong type (for example, calling a personal name an
organization)
assigning it a related but inexact type (for example, "substance" vs. "drug", or
"school" vs. "organization")
correctly identifying an entity, when what the user wanted was a smaller- or
larger-scope entity (for example, identifying "James Madison" as a personal name, when
it's part of "James Madison University"). Some NER systems impose the restriction that
entities may never overlap or nest, which means that in some cases one must make
arbitrary or task-specific choices.
One overly simple method of measuring accuracy is merely to count what fraction of all
tokens in the text were correctly or incorrectly identified as part of entity references
(or as being entities of the correct type). This suffers from at least two problems:
first, the vast majority of tokens in real-world text are not part of entity names, so
the baseline accuracy (always predict "not an entity") is extravagantly high, typically
>90%; and second, mispredicting the full span of an entity name is not properly
penalized (finding only a person's first name when his last name follows might be scored
as ½ accuracy).
In academic conferences such as CoNLL, a variant of the F1 score has been defined as
follows:

Precision is the number of predicted entity name spans that line up exactly with spans
in the gold standard evaluation data. I.e. when [Person Hans] [Person Blick] is
predicted but [Person Hans Blick] was required, precision for the predicted name is
zero. Precision is then averaged over all predicted entity names.
Recall is similarly the number of names in the gold standard that appear at exactly the
same location in the predictions.
F1 score is the harmonic mean of these two.
It follows from the above definition that any prediction that misses a single token,
includes a spurious token, or has the wrong class, is a hard error and does not
contribute positively to either precision or recall. Thus, this measure may be said to
be pessimistic: it can be the case that many "errors" are close to correct, and might be
adequate for a given purpose. For example, one system might always omit titles such as
"Ms." or "Ph.D.", but be compared to a system or ground-truth data that expects titles
to be included. In that case, every such name is treated as an error. Because of such
issues, it is important actually to examine the kinds of errors, and decide how
important they are given one's goals and requirements.
Evaluation models based on a token-by-token matching have been proposed. Such models may
be given partial credit for overlapping matches (such as using the Intersection over
Union criterion). They allow a finer grained evaluation and comparison of extraction
systems.


== Approaches ==
NER systems have been created that use linguistic grammar-based techniques as well as
statistical models such as machine learning. State of the art systems may incorporate
multiple approaches.

GATE supports NER across many languages and domains out of the box, usable via a
graphical interface and a Java API.
OpenNLP includes rule-based and statistical named-entity recognition.
spaCy features fast statistical NER as well as an open-source named-entity visualizer.
Hand-crafted grammar-based systems typically obtain better precision, but at the cost of
lower recall and months of work by experienced computational linguists.
Statistical NER systems typically require a large amount of manually annotated training
data. Semisupervised approaches have been suggested to avoid part of the annotation
effort.
In the statistical learning era, NER was usually performed by learning a simple linear
regression model on engineered features, then decoded by a bidirectional Viterbi
algorithm. Some commonly used features include:

Lexical items: The token itself be labeled.
Stemmed lexical items.
Shape: The orthographic pattern of the target word. For example, all lowercase, all
uppercase, initial uppercase, mixed case, uppercase followed by a period (often
indicating a middle name), contains hyphen, etc.
Affixes of the target word and surrounding words.
Part of speech of the word.
Whether the word appears in one or more named entity lists (gazetteers).
Words and/or n-grams occurring in the surrounding context.
A gazetteer is a list of names and their types, such as "General Electric". It can be
used to augment any system for NER. They had been often used in the era of statistical
machine learning.
Many different classifier types have been used to perform machine-learned NER, with
conditional random fields being a typical choice. Transformers features token
classification using deep learning models.


== History ==

Early work in NER systems in the 1990s was aimed primarily at extraction from
journalistic articles. Attention then turned to processing of military dispatches and
reports. Later stages of the automatic content extraction (ACE) evaluation also included
several types of informal text styles, such as weblogs and text transcripts from
conversational telephone speech conversations. Since about 1998, there has been a great
deal of interest in entity identification in the molecular biology, bioinformatics, and
medical natural language processing communities.  The most common entity of interest in
that domain has been names of genes and gene products. There has been also considerable
interest in the recognition of chemical entities and drugs in the context of the
CHEMDNER
competition, with 27 teams participating in this task.
In 2001, research indicated that even state-of-the-art NER systems were brittle, meaning
that NER systems developed for one domain did not typically perform well on other
domains. Considerable effort is involved in tuning NER systems to perform well in a new
domain; this is true for both rule-based and trainable statistical systems.
As of 2007, state-of-the-art NER systems for English produce near-human performance. For
example, the best system entering MUC-7 scored 93.39% of F-measure while human
annotators scored 97.60% and 96.95%.


=== Current challenges ===
Despite high F1 numbers reported on the MUC-7 dataset, the problem of named-entity
recognition is far from being solved. The main efforts are directed to reducing the
annotations labor by employing semi-supervised learning, robust performance across
domains and scaling up to fine-grained entity types. In recent years, many projects have
turned to crowdsourcing, which is a promising solution to obtain high-quality aggregate
human judgments for supervised and semi-supervised machine learning approaches to NER.
Another challenging task is devising models to deal with linguistically complex contexts
such as Twitter and search queries.
There are some researchers who did some comparisons about the NER performances from
different statistical models such as HMM (hidden Markov model), ME (maximum entropy),
and CRF (conditional random fields), and feature sets. And some researchers recently
proposed graph-based semi-supervised learning model for language specific NER tasks.
A recently emerging task of identifying "important expressions" in text and
cross-linking them to Wikipedia can be seen as an instance of extremely fine-grained
named-entity recognition, where the types are the actual Wikipedia pages describing the
(potentially ambiguous) concepts. Below is an example output of a Wikification system:

Another field that has seen progress but remains challenging is the application of NER
to Twitter and other microblogs, considered "noisy" due to non-standard orthography,
shortness and informality of texts. NER challenges in English Tweets have been organized
by research communities to compare performances of various approaches, such as
bidirectional LSTMs, Learning-to-Search, or CRFs.


== See also ==
Controlled vocabulary
Coreference resolution
Entity linking (aka named entity normalization, entity disambiguation)
Information extraction
Knowledge extraction
Onomastics
Record linkage
Semantic Web
Smart tag (Microsoft)


== References ==

Jurafsky, Daniel; Martin, James H. (2008). "13.5 Partial Parsing". Speech and Language
Processing (2nd ed.). Upper Saddle River, N.J.: Prentice Hall. ISBN 978-0131873216.
Jurafsky, Daniel; Martin, James H. (2008). "22.1. Named Entity Recognition". Speech and
Language Processing (2nd ed.). Upper Saddle River, N.J.: Prentice Hall. ISBN
978-0131873216.
