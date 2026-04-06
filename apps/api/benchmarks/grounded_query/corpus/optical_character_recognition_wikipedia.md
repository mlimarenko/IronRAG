# Optical character recognition

Source: https://en.wikipedia.org/wiki/Optical_character_recognition
Source type: Wikipedia plaintext extract truncated to approximately 230 wrapped lines

Optical character recognition (OCR) or optical character reader is the electronic or
mechanical conversion of images of typed, handwritten or printed text into
machine-encoded text, whether from a scanned document, a photo of a document, a scene
photo (for example the text on signs and billboards in a landscape photo) or from
subtitle text superimposed on an image (for example: from a television broadcast).
Widely used as a form of data entry from printed paper data records – whether passport
documents, invoices, bank statements, computerized receipts, business cards, mail,
printed data, or any suitable documentation – it is a common method of digitizing
printed texts so that they can be electronically edited, searched, stored more
compactly, displayed online, and used in machine processes such as cognitive computing,
machine translation, (extracted) text-to-speech, key data and text mining. OCR is a
field of research in pattern recognition, artificial intelligence and computer vision.
Early versions needed to be trained with images of each character, and worked on one
font at a time. Advanced systems capable of producing a high degree of accuracy for most
fonts are now common, and with support for a variety of image file format inputs. Some
systems are capable of reproducing formatted output that closely approximates the
original page including images, columns, and other non-textual components.


== History ==

Early optical character recognition may be traced to technologies involving telegraphy
and creating reading devices for the blind. In 1914, Emanuel Goldberg developed a
machine that read characters and converted them into standard telegraph code.
Concurrently, Edmund Fournier d'Albe developed the Optophone, a handheld scanner that
when moved across a printed page, produced tones that corresponded to specific letters
or characters.
In the late 1920s and into the 1930s, Emanuel Goldberg developed what he called a
"Statistical Machine" for searching microfilm archives using an optical code recognition
system. In 1931, he was granted US Patent number 1,838,389 for the invention. The patent
was acquired by IBM.


=== Visually impaired users ===
In 1974, Ray Kurzweil started the company Kurzweil Computer Products, Inc. and continued
development of omni-font OCR, which could recognize text printed in virtually any font.
(Kurzweil is often credited with inventing omni-font OCR, but it was in use by
companies, including CompuScan, in the late 1960s and 1970s.) Kurzweil used the
technology to create a reading machine for blind people to have a computer read text to
them out loud. The device included a CCD-type flatbed scanner and a text-to-speech
synthesizer. On January 13, 1976, the finished product was unveiled during a widely
reported news conference headed by Kurzweil and the leaders of the National Federation
of the Blind. In 1978, Kurzweil Computer Products began selling a commercial version of
the optical character recognition computer program. LexisNexis was one of the first
customers, and bought the program to upload legal paper and news documents onto its
nascent online databases. Two years later, Kurzweil sold his company to Xerox, which
eventually spun it off as Scansoft, which merged with Nuance Communications.
In the 2000s, OCR was made available online as a service (WebOCR), in a cloud computing
environment, and in mobile applications like real-time translation of foreign-language
signs on a smartphone. With the advent of smartphones and smartglasses, OCR can be used
in internet connected mobile device applications that extract text captured using the
device's camera. These devices that do not have built-in OCR functionality will
typically use an OCR API to extract the text from the image file captured by the device.
The OCR API returns the extracted text, along with information about the location of the
detected text in the original image back to the device app for further processing (such
as text-to-speech) or display.
Various commercial and open source OCR systems are available for most common writing
systems, including Latin, Cyrillic, Arabic, Hebrew, Indic, Bengali (Bangla), Devanagari,
Tamil, Chinese, Japanese, and Korean characters.


== Applications ==
OCR engines have been developed into software applications specializing in various
subjects such as receipts, invoices, checks, and legal billing documents.
The software can be used for:

Entering data for business documents, e.g. checks, passports, invoices, bank statements
and receipts
Automatic number-plate recognition
Passport recognition and information extraction in airports
Automatically extracting key information from insurance documents
Traffic-sign recognition
Extracting business card information into a contact list
Creating textual versions of printed documents, e.g. book scanning for Project Gutenberg
Making electronic images of printed documents searchable, e.g. Google Books
Converting handwriting in real-time to control a computer (pen computing)
Defeating or testing the robustness of CAPTCHA anti-bot systems, though these are
specifically designed to prevent OCR.
Assistive technology for blind and visually impaired users
Writing instructions for vehicles by identifying CAD images in a database that are
appropriate to the vehicle design as it changes in real time
Making scanned documents searchable by converting them to PDFs


== Types ==
Optical character recognition (OCR) – targets typewritten text, one glyph or character
at a time.
Optical word recognition – targets typewritten text, one word at a time (for languages
that use a space as a word divider). Usually just called "OCR".
Intelligent character recognition (ICR) – also targets handwritten printscript or
cursive text one glyph or character at a time, usually involving machine learning.
Intelligent word recognition (IWR) – also targets handwritten printscript or cursive
text, one word at a time. This is especially useful for languages where glyphs are not
separated in cursive script.
OCR is generally an offline process, which analyses a static document. There are cloud
based services which provide an online OCR API service. Handwriting movement analysis
can be used as input to handwriting recognition. Instead of merely using the shapes of
glyphs and words, this technique is able to capture motion, such as the order in which
segments are drawn, the direction, and the pattern of putting the pen down and lifting
it. This additional information can make the process more accurate. This technology is
also known as "online character recognition", "dynamic character recognition",
"real-time character recognition", and "intelligent character recognition".


== Techniques ==


=== Pre-processing ===
OCR software often pre-processes images to improve the chances of successful
recognition. Techniques include:

De-skewing – if the document was not aligned properly when scanned, it may need to be
tilted a few degrees clockwise or counterclockwise in order to make lines of text
perfectly horizontal or vertical.
Despeckling – removal of positive and negative spots, smoothing edges
Binarization – conversion of an image from color or greyscale to black-and-white (called
a binary image because there are two colors). The task is performed as a simple way of
separating the text (or any other desired image component) from the background. The task
of binarization is necessary since most commercial recognition algorithms work only on
binary images, as it is simpler to do so. In addition, the effectiveness of binarization
influences to a significant extent the quality of character recognition, and careful
decisions are made in the choice of the binarization employed for a given input image
type; since the quality of the method used to obtain the binary result depends on the
type of image (scanned document, scene text image, degraded historical document, etc.).
Line removal – Cleaning up non-glyph boxes and lines
Layout analysis or zoning – Identification of columns, paragraphs, captions, etc. as
distinct blocks. Especially important in multi-column layouts and tables.
Line and word detection – Establishment of a baseline for word and character shapes,
separating words as necessary.
Script recognition – In multilingual documents, the script may change at the level of
the words and hence, identification of the script is necessary, before the right OCR can
be invoked to handle the specific script.
Character isolation or segmentation – For per-character OCR, multiple characters that
are connected due to image artifacts must be separated; single characters that are
broken into multiple pieces due to artifacts must be connected.
Normalization of aspect ratio and scale
Segmentation of fixed-pitch fonts is accomplished relatively simply by aligning the
image to a uniform grid based on where vertical grid lines will least often intersect
black areas. For proportional fonts, more sophisticated techniques are needed because
whitespace between letters can sometimes be greater than that between words, and
vertical lines can intersect more than one character.


=== Text recognition ===
There are two basic types of core OCR algorithm, which may produce a ranked list of
candidate characters.

Matrix matching involves comparing an image to a stored glyph on a pixel-by-pixel basis;
it is also known as pattern matching, pattern recognition, or image correlation. This
relies on the input glyph being correctly isolated from the rest of the image, and the
stored glyph being in a similar font and at the same scale. This technique works best
with typewritten text and does not work well when new fonts are encountered. This is the
technique early physical photocell-based OCR implemented, rather directly.
Feature extraction decomposes glyphs into "features" like lines, closed loops, line
direction, and line intersections. The extraction features reduces the dimensionality of
the representation and makes the recognition process computationally efficient. These
features are compared with an abstract vector-like representation of a character, which
might reduce to one or more glyph prototypes. General techniques of feature detection in
computer vision are applicable to this type of OCR, which is commonly seen in
"intelligent" handwriting recognition and most modern OCR software. Nearest neighbour
classifiers such as the k-nearest neighbors algorithm are used to compare image features
with stored glyph features and choose the nearest match.
Software such as Cuneiform and Tesseract use a two-pass approach to character
recognition. The second pass is known as adaptive recognition and uses the letter shapes
recognized with high confidence on the first pass to better recognize the remaining
letters on the second pass. This is advantageous for unusual fonts or low-quality scans
where the font is distorted (e.g. blurred or faded).
As of 2024, modern OCR software includes Google Docs OCR, ABBYY FineReader, Transym, and
open source engines like Tesseract 5 (which introduced an LSTM-based recognition
engine), PaddleOCR (a multilingual OCR toolkit supporting over 80 languages), and TrOCR
(a transformer-based model developed by Microsoft for handwritten and printed text
recognition). Others like OCRopus and Tesseract use neural networks which are trained to
recognize whole lines of text instead of focusing on single characters.
A technique known as iterative OCR automatically crops a document into sections based on
the page layout. OCR is then performed on each section individually using variable
character confidence level thresholds to maximize page-level OCR accuracy. A patent from
the United States Patent Office has been issued for this method.
The OCR result can be stored in the standardized ALTO format, a dedicated XML schema
maintained by the United States Library of Congress. Other common formats include hOCR
and PAGE XML.
For a list of optical character recognition software, see Comparison of optical
character recognition software.


=== Post-processing ===
OCR accuracy can be increased if the output is constrained by a lexicon – a list of
words that are allowed to occur in a document. This might be, for example, all the words
in the English language, or a more technical lexicon for a specific field. This
technique can be problematic if the document contains words not in the lexicon, like
proper nouns. Tesseract uses its dictionary to influence the character segmentation
step, for improved accuracy.
The output stream may be a plain text stream or file of characters, but more
sophisticated OCR systems can preserve the original layout of the page and produce, for
example, an annotated PDF that includes both the original image of the page and a
searchable textual representation.
Near-neighbor analysis can make use of co-occurrence frequencies to correct errors, by
noting that certain words are often seen together. For example, "Washington, D.C." is
generally far more common in English than "Washington DOC".
Knowledge of the grammar of the language being scanned can also help determine if a word
is likely to be a verb or a noun, for example, allowing greater accuracy.
The Levenshtein Distance algorithm has also been used in OCR post-processing to further
optimize results from an OCR API.


=== Application-specific optimizations ===
In recent years, the major OCR technology providers began to tweak OCR systems to deal
more efficiently with specific types of input. Beyond an application-specific lexicon,
better performance may be had by taking into account business rules, standard
expression, or rich information contained in color images. This strategy is called
"Application-Oriented OCR" or "Customized OCR", and has been applied to OCR of license
plates, invoices, screenshots, ID cards, driver's licenses, and automobile
manufacturing.
The New York Times has adapted the OCR technology into a proprietary tool they entitle
Document Helper, that enables their interactive news team to accelerate the processing
of documents that need to be reviewed. They note that it enables them to process what
amounts to as many as 5,400 pages per hour in preparation for reporters to review the
contents.


== Workarounds ==
There are several techniques for solving the problem of character recognition by means
other than improved OCR algorithms.


=== Forcing better input ===
Special fonts like OCR-A, OCR-B, or MICR fonts, with precisely specified sizing,
spacing, and distinctive character shapes, allow a higher accuracy rate during
transcription in bank check processing. Several prominent OCR engines were designed to
capture text in popular fonts such as Arial or Times New Roman, and are incapable of
capturing text in these fonts that are specialized and very different from popularly
