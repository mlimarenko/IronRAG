# Large language model

Source: https://en.wikipedia.org/wiki/Large_language_model
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

A large language model (LLM) is a computational model trained on a vast amount of data,
designed for natural language processing tasks, especially language generation. The
largest and most capable LLMs are generative pre-trained transformers (GPTs) that
provide the core capabilities of modern chatbots. LLMs use large numbers of artificial
intelligence parameters to generate, summarize, translate and reason over text in a
large variety of contexts. Models may be fine-tuned to perform specific tasks, or users
can give them more specific prompts to refine their output. As LLMs are trained on
collections of human-written text, they are able to reflect patterns in natural
language. They may include inaccuracies and biases which the humans who produced the
training data held.
Compared to earlier statistical and recurrent neural network approaches to language
modeling, LLMs use a transformer architecture. This allows for more efficient
parallelization, longer context handling, and scalable training using higher data
volumes. Models like GPT, BERT, and their successors used these advances to demonstrate
emergent behaviors at scale, such as finding specific data from a large data set and
compositional reasoning.
Benchmark evaluations for LLMs test a model's ability to perform one or more language
tasks. Modern LLMs may face comprehensive, multi-task evaluations measuring reasoning,
factual accuracy, alignment, and safety. Optimizing training sessions to pass benchmarks
may result in a model that adheres too closely to benchmark outputs without genuine
generalization or robust capability improvements.


== History ==

Before the emergence of transformer-based models in 2017, some language models were
considered large relative to the computational and data constraints of their time. In
the early 1990s, IBM's statistical models pioneered word alignment techniques for
machine translation, laying the groundwork for corpus-based language modeling. In 2001,
a smoothed n-gram model, such as those employing Kneser–Ney smoothing, trained on 300
million words, achieved state-of-the-art perplexity on benchmark tests. During the
2000s, with the rise of widespread internet access, researchers began compiling massive
text datasets from the web ("web as corpus") to train statistical language models.

Moving beyond n-gram models, researchers started in 2000 to use neural networks to learn
language models. Following the breakthrough of deep neural networks in image
classification around 2012, similar architectures were adapted for language tasks. This
shift was marked by the development of word embeddings (e.g., Word2Vec by Mikolov in
2013) and sequence-to-sequence (seq2seq) models using LSTM. In 2016, Google transitioned
its translation service to neural machine translation (NMT), replacing statistical
phrase-based models with deep recurrent neural networks. These early NMT systems used
LSTM-based encoder-decoder architectures, as they preceded the invention of
transformers.
At the 2017 NeurIPS conference, Google researchers introduced the transformer
architecture in their landmark paper "Attention Is All You Need". This paper's goal was
to improve upon 2014 seq2seq technology, and was based mainly on the attention mechanism
developed by Bahdanau et al. in 2014. The following year in 2018, BERT was introduced
and quickly became "ubiquitous". Though the original transformer has both encoder and
decoder blocks, BERT is an encoder-only model. Academic and research usage of BERT began
to decline in 2023, following rapid improvements in the abilities of decoder-only models
(such as GPT) to solve tasks via prompting.
Although decoder-only GPT-1 was introduced in 2018, it was GPT-2 in 2019 that caught
widespread attention because OpenAI claimed to have initially deemed it too powerful to
release publicly, out of fear of malicious use. GPT-3 in 2020 went a step further and as
of 2025 is available only via API with no offering of downloading the model to execute
locally. But it was the 2022 consumer-facing chatbot ChatGPT that received extensive
media coverage and public attention. The 2023 GPT-4 was praised for its increased
accuracy and as a "holy grail" for its multimodal capabilities. OpenAI did not reveal
the high-level architecture and the number of parameters of GPT-4. The release of
ChatGPT led to an uptick in LLM usage across several research subfields of computer
science, including robotics, software engineering, and societal impact work. In 2024
OpenAI released the reasoning model OpenAI o1, which generates long chains of thought
before returning a final answer. Many LLMs with parameter counts comparable to those of
OpenAI's GPT series have been developed.
Since 2022, open-weight models have been gaining popularity, especially at first with
BLOOM and LLaMA, though both have restrictions on usage and deployment. Mistral AI's
models Mistral 7B and Mixtral 8x7b have a more permissive Apache License. In January
2025, DeepSeek released DeepSeek R1, a 671-billion-parameter open-weight model that
performs comparably to OpenAI o1 but at a much lower price per token for users.
Since 2023, many LLMs have been trained to be multimodal, having the ability to also
process or generate other types of data, such as images, audio, or 3D meshes. These LLMs
are also called large multimodal models (LMMs), or multimodal large language models
(MLLMs).
As of 2024, the largest and most capable models are all based on the transformer
architecture. Some recent implementations are based on other architectures, such as
recurrent neural network variants and Mamba (a state space model).
Open-weight LLMs have increasingly shaped the field since 2023, contributing to broader
participation in AI development and greater transparency in model evaluation. Vake et
al. (2025) demonstrated that community-driven contributions to open-weight models
measurably improve their efficiency and performance, with user participation growing
rapidly on collaborative platforms such as Hugging Face. Paris et al. (2025) further
argued that openness in AI should extend beyond releasing model code or weights to
encompass inclusiveness, accountability, and ethical responsibility in AI research and
deployment. Collectively, these studies highlight that open-weight LLMs can accelerate
innovation and enhance scientific reproducibility, while fostering a more transparent
and participatory AI ecosystem.


== Dataset preprocessing ==


=== Tokenization ===
As machine learning algorithms process numbers rather than text, the text must be
converted to numbers. In the first step, a vocabulary is decided upon, then integer
indices are arbitrarily but uniquely assigned to each vocabulary entry, and finally, an
embedding is associated to the integer index. Algorithms include byte-pair encoding
(BPE) and WordPiece. There are also special tokens serving as control characters, such
as [MASK] for masked-out token (as used in BERT), and [UNK] ("unknown") for characters
not appearing in the vocabulary. Also, some special symbols are used to denote special
text formatting. For example, "Ġ" denotes a preceding whitespace in RoBERTa and GPT and
"##" denotes continuation of a preceding word in BERT.
For example, the BPE tokenizer used by the legacy version of GPT-3 would split
tokenizer: texts -> series of numerical "tokens" as

Tokenization also compresses the datasets. Because LLMs generally require input to be an
array that is not jagged, the shorter texts must be "padded" until they match the length
of the longest one. The average number of words per token depends on the language.


==== Byte-pair encoding ====

As an example, consider a tokenizer based on byte-pair encoding. In the first step, all
unique characters (including blanks and punctuation marks) are treated as an initial set
of n-grams (i.e. initial set of uni-grams). Successively the most frequent pair of
adjacent characters is merged into a bi-gram and all instances of the pair are replaced
by it. All occurrences of adjacent pairs of (previously merged) n-grams that most
frequently occur together are then again merged into even lengthier n-gram, until a
vocabulary of prescribed size is obtained. After a tokenizer is trained, any text can be
tokenized by it, as long as it does not contain characters not appearing in the
initial-set of uni-grams.


==== Problems ====
A token vocabulary based on the frequencies extracted from mainly English corpora uses
as few tokens as possible for an average English word. However, an average word in
another language encoded by such an English-optimized tokenizer is split into a
suboptimal amount of tokens. GPT-2 tokenizer can use up to 15 times more tokens per word
for some languages, for example for the Shan language from Myanmar. Even more widespread
languages such as Portuguese and German have "a premium of 50%" compared to English.


=== Dataset cleaning ===

In the context of training LLMs, datasets are typically cleaned by removing low-quality,
duplicated, or toxic data. Cleaned datasets can increase training efficiency and lead to
improved downstream performance. A trained LLM can be used to clean datasets for
training a further LLM.
With the increasing proportion of LLM-generated content on the web, data cleaning in the
future may include filtering out such content. LLM-generated content can pose a problem
if the content is similar to human text (making filtering difficult) but of lower
quality (degrading performance of models trained on it).


=== Synthetic data ===

Training of largest language models might need more linguistic data than naturally
available, or that the naturally occurring data is of insufficient quality. In these
cases, synthetic data might be used. Microsoft's Phi series of LLMs is trained on
textbook-like data generated by another LLM.


== Training ==

An LLM is a type of foundation model (large X model) trained on language. LLMs can be
trained in different ways. In particular, GPT models are first pretrained to predict the
next word on a large amount of data, before being fine-tuned.


=== Cost ===

Substantial infrastructure is necessary for training the largest models. The tendency
towards larger models is visible in the list of large language models. For example, the
training of GPT-2 (i.e. a 1.5-billion-parameter model) in 2019 cost $50,000, while
training of the PaLM (i.e. a 540-billion-parameter model) in 2022 cost $8 million, and
Megatron-Turing NLG 530B (in 2021) cost around $11 million. The qualifier "large" in
"large language model" is inherently vague, as there is no definitive threshold for the
number of parameters required to qualify as "large". GPT-1 of 2018 has 117 million
parameters.


=== Fine-tuning ===
Before being fine-tuned, most LLMs are next-token predictors. The fine-tuning shapes the
LLM's behavior via techniques like reinforcement learning from human feedback (RLHF) or
constitutional AI.
Instruction fine-tuning is a form of supervised learning used to teach LLMs to follow
user instructions. In 2022, OpenAI demonstrated InstructGPT, a version of GPT-3
similarly fine-tuned to follow instructions.
Reinforcement learning from human feedback (RLHF) involves training a reward model to
predict which text humans prefer. Then, the LLM can be fine-tuned through reinforcement
learning to better satisfy this reward model. Since humans typically prefer truthful,
helpful and harmless answers, RLHF favors such answers.


== Architecture ==
LLMs are generally based on the transformer architecture, which leverages an attention
mechanism that enables the model to process relationships between all elements in a
sequence simultaneously, regardless of their distance from each other.


=== Attention mechanism and context window ===

In order to find out which tokens are relevant to each other within the scope of the
context window, the attention mechanism calculates "soft" weights for each token, more
precisely for its embedding, by using multiple attention heads, each with its own
"relevance" for calculating its own soft weights. For example, the small (i.e. 117M
parameter sized) GPT-2 model has had twelve attention heads and a context window of only
1k tokens. In its medium version it has 345M parameters and contains 24 layers, each
with 12 attention heads. For the training with gradient descent a batch size of 512 was
utilized.
Google's Gemini 1.5, introduced in February 2024, can have a context window of up to 1
million tokens.
A model may be pre-trained either to predict how the segment continues, or what is
missing in the segment, given a segment from its training dataset. It can be either

autoregressive (i.e. predicting how the segment continues, as GPTs do): for example
given a segment "I like to eat", the model predicts "ice cream", or "sushi".
"masked" (i.e. filling in the parts missing from the segment, the way "BERT" does it):
for example, given a segment "I like to [__] [__] cream", the model predicts that "eat"
and "ice" are missing.
Models may be trained on auxiliary tasks which test their understanding of the data
distribution, such as next sentence prediction (NSP), in which pairs of sentences are
presented and the model must predict whether they appear consecutively in the training
corpus. During training, regularization loss is also used to stabilize training.
However, regularization loss is usually not used during testing and evaluation.


=== Mixture of experts ===

A mixture of experts (MoE) is a machine learning architecture in which multiple
specialized neural networks ("experts") work together, with a gating mechanism that
routes each input to the most appropriate expert(s). Mixtures of experts can reduce
inference costs, as only a fraction of the parameters are used for each input. The
approach was introduced in 2017 by Google researchers.


=== Parameter size ===

Typically, LLMs are trained with single- or half-precision floating point numbers
(float32 and float16). One float16 has 16 bits, or 2 bytes, and so one billion
parameters require 2 gigabytes. The largest models typically have more than 100 billion
