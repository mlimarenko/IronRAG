# Transformer (deep learning)

Source: https://en.wikipedia.org/wiki/Transformer_(deep_learning_architecture)
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

In deep learning, the transformer is a family of artificial neural network architectures
based on the multi-head attention mechanism, in which text is converted to numerical
representations called tokens, and each token is converted into a vector via lookup from
a word embedding table. At each layer, each token is then contextualized within the
scope of the context window with other (unmasked) tokens via a parallel multi-head
attention mechanism, allowing the signal for key tokens to be amplified and less
important tokens to be diminished.
Transformers have the advantage of having no recurrent units, therefore requiring less
training time than earlier recurrent neural architectures (RNNs) such as long short-term
memory (LSTM). Later variations have been widely adopted for training large language
models (LLMs) on large (language) datasets.

The original version of the transformer architecture was proposed in the 2017 paper
"Attention Is All You Need" by researchers at Google. The predecessors of transformers
were developed as an improvement over previous architectures for machine translation,
but have found many applications since. They are used in large-scale natural language
processing, computer vision (vision transformers), reinforcement learning, audio,
multimodal learning, robotics, and playing chess. It has also led to the development of
pre-trained systems, such as generative pre-trained transformers (GPTs) and BERT
(bidirectional encoder representations from transformers).


== History ==


=== Predecessors ===
For many years, sequence modelling and generation was done by using plain recurrent
neural networks (RNNs). A well-cited early example was the Elman network (1990). In
theory, the information from one token can propagate arbitrarily far down the sequence,
but in practice the vanishing-gradient problem leaves the model's state at the end of a
long sentence without precise, extractable information about preceding tokens.
A key breakthrough was LSTM (1995), an RNN which used various innovations to overcome
the vanishing gradient problem, allowing efficient learning of long-sequence modelling.
One key innovation was the use of an attention mechanism which used neurons that
multiply the outputs of other neurons, so-called multiplicative units. Neural networks
using multiplicative units were later called sigma-pi networks or higher-order networks.
LSTM became the standard architecture for long sequence modelling until the 2017
publication of transformers. However, LSTM still used sequential processing, like most
other RNNs. Specifically, RNNs operate one token at a time from first to last; they
cannot operate in parallel over all tokens in a sequence.
Modern transformers overcome this problem, but unlike RNNs, they require computation
time that is quadratic in the size of the context window. The linearly scaling fast
weight controller (1992) learns to compute a weight matrix for further processing
depending on the input. One of its two networks has "fast weights" or "dynamic links"
(1981). A slow neural network learns by gradient descent to generate keys and values for
computing the weight changes of the fast neural network which computes answers to
queries. This was later shown to be equivalent to the unnormalized linear transformer.


=== Attention with seq2seq ===

The idea of encoder–decoder sequence transduction had been developed in the early 2010s;
commonly cited as the originators that produced seq2seq are two concurrently published
papers from 2014.
A 380M-parameter model for machine translation uses two long short-term memories (LSTM).
Its architecture consists of two parts. The encoder is an LSTM that takes in a sequence
of tokens and turns it into a vector. The decoder is another LSTM that converts the
vector into a sequence of tokens. Similarly, another 130M-parameter model used gated
recurrent units (GRU) instead of LSTM. Later research showed that GRUs are neither
better nor worse than LSTMs for seq2seq.
These early seq2seq models had no attention mechanism, and the state vector is
accessible only after the last word of the source text was processed. Although in theory
such a vector retains the information about the whole original sentence, in practice the
information is poorly preserved. This is because the input is processed sequentially by
one recurrent network into a fixed-size output vector, which is then processed by
another recurrent network into an output. If the input is long, then the output vector
would not be able to contain all relevant information, degrading the output. As
evidence, reversing the input sentence improved seq2seq translation.
The RNN search model introduced an attention mechanism to seq2seq for machine
translation to solve the bottleneck problem (of the fixed-size output vector), allowing
the model to process long-distance dependencies more easily. The name is because it
"emulates searching through a source sentence during decoding a translation".
The relative performances were compared between global (that of RNN search) and local
(sliding window) attention model architectures for machine translation, finding that
mixed attention had higher quality than global attention, while local attention reduced
translation time.
In 2016, Google Translate was revamped to Google Neural Machine Translation, which
replaced the previous model based on statistical machine translation. The new model was
a seq2seq model where the encoder and the decoder were both 8 layers of bidirectional
LSTM. It took nine months to develop, and it outperformed the statistical approach,
which took ten years to develop.


=== Parallelizing attention ===

Seq2seq models with attention (including self-attention) still suffered from the same
issue with recurrent networks, which is that they are hard to parallelize, which
prevented them from being accelerated on GPUs. In 2016, decomposable attention applied a
self-attention mechanism to feedforward networks, which are easy to parallelize, and
achieved SOTA result in textual entailment with an order of magnitude fewer parameters
than LSTMs. One of its authors, Jakob Uszkoreit, suspected that attention without
recurrence would be sufficient for language translation, thus the title "attention is
all you need". That hypothesis was against conventional wisdom at the time, and even his
father Hans Uszkoreit, a well-known computational linguist, was skeptical. In the same
year, self-attention (called intra-attention or intra-sentence attention) was proposed
for LSTMs.
On 2017-06-12, the original (100M-parameter) encoder–decoder transformer model was
published in the "Attention is all you need" paper. At the time, the focus of the
research was on improving seq2seq for machine translation, by removing its recurrence to
process all tokens in parallel, but preserving its dot-product attention mechanism to
keep its text processing performance. This led to the introduction of a multi-head
attention model that was easier to parallelize due to the use of independent heads and
the lack of recurrence. Its parallelizability was an important factor to its widespread
use in large neural networks.


=== AI boom era ===
As early as spring 2017, even before the "Attention is all you need" preprint was
published, one of the co-authors applied the "decoder-only" variation of the
architecture to generate fictitious Wikipedia articles. Transformer architecture is now
used alongside many generative models that contribute to the ongoing AI boom.
The "reference implementation" of the original Transformer was written in a TensorFlow
library. In language modelling, ELMo (2018) was a bi-directional LSTM that produces
contextualized word embeddings, improving upon the line of research from bag of words
and word2vec. It was followed by BERT (2018), an encoder-only transformer model. In
October 2019, Google started using BERT to process search queries. In 2020, Google
Translate replaced the previous RNN-encoder–RNN-decoder model by a
transformer-encoder–RNN-decoder model.
Starting in 2018, the OpenAI GPT series of decoder-only transformers became state of the
art in natural language generation. In the end of 2022, a chatbot based on GPT-3,
ChatGPT, became unexpectedly popular, triggering a boom around large language models.
Transformers have been applied in modalities beyond text. 4 days after the publication
of "Attention is All You Need", a multimodal transformer architecture, MultiModel, was
published by most authors of that paper. Other examples include the vision transformer,
speech recognition, robotics, and multimodal. The vision transformer, in turn,
stimulated new developments in convolutional neural networks. Image and video generators
like DALL-E (2021), Stable Diffusion 3 (2024), and Sora (2024), use transformers to
analyse input data (like text prompts) by breaking it down into "tokens" and then
calculating the relevance between each token using self-attention, which helps the model
understand the context and relationships within the data.


== Training ==


=== Methods for stabilizing training ===
The plain transformer architecture had difficulty in converging. In the original paper,
the authors recommended using learning rate warmup. That is, the learning rate should
linearly scale up from 0 to maximal value for the first part of the training (usually
recommended to be 2% of the total number of training steps), before decaying again.
A 2020 paper found that using layer normalization before (instead of after) multihead
attention and feedforward layers stabilizes training, not requiring learning rate
warmup. This is the "pre-LN Transformer" and is more commonly used, compared to the
original "post-LN Transformer".


=== Pretrain-finetune ===
Transformers typically are first pretrained by self-supervised learning on a large
generic dataset, followed by supervised fine-tuning on a small task-specific dataset.
The pretrain dataset is typically an unlabeled large corpus, such as The Pile. Tasks for
pretraining and fine-tuning commonly include:

language modeling
next-sentence prediction
question answering
reading comprehension
sentiment analysis
paraphrasing
The T5 transformer report documents a large number of natural language pretraining
tasks. Some examples are:

restoring or repairing incomplete or corrupted text. For example, the input, "Thank
you ~~ me to your party ~~ week", might generate the output, "Thank you for inviting me
to your party last week".
translation between natural languages (machine translation)
judging the pragmatic acceptability of natural language. For example, the following
sentence might be judged "not acceptable", because even though it is syntactically
well-formed, it is improbable in ordinary human usage: The course is jumping well.
Note that while each of these tasks is trivial or obvious for human native speakers of
the language (or languages), they have typically proved challenging for previous
generations of machine learning architecture.


=== Tasks ===

In general, there are 3 classes of language modelling tasks: "masked", "autoregressive",
and "prefixLM". These classes are independent of a specific modeling architecture such
as transformer, but they are often discussed in the context of transformer.
In a masked task, one or more of the tokens is masked out, and the model would produce a
probability distribution predicting what the masked-out tokens are based on the context.
The loss function for the task is typically sum of log-perplexities for the masked-out
tokens:




Loss

=
−

∑

t
∈

masked tokens



ln
⁡
(

probability of

t

conditional on its context

)


{\displaystyle {\text{Loss}}=-\sum _{t\in {\text{masked tokens}}}\ln({\text{probability
of }}t{\text{ conditional on its context}})}

and the model is trained to minimize this loss function. The BERT series of models are
trained for masked token prediction and another task.
In an autoregressive task, the entire sequence is masked at first, and the model
produces a probability distribution for the first token. Then the first token is
revealed and the model predicts the second token, and so on. The loss function for the
task is still typically the same. The GPT series of models are trained by autoregressive
tasks.
In a prefixLM task, the sequence is divided into two parts. The first part is presented
as context, and the model predicts the first token of the second part. Then that would
be revealed, and the model predicts the second token, and so on. The loss function for
the task is still typically the same. The T5 series of models are trained by prefixLM
tasks.
Note that "masked" as in "masked language modelling" is not "masked" as in "masked
attention", and "prefixLM" as in
