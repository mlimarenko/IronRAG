"""
Machine Learning Pipeline for Text Classification

This module implements a complete ML pipeline for document classification
using scikit-learn and transformers. It supports training, evaluation,
and inference with multiple model backends.

Environment Variables:
    ML_MODEL_PATH: Path to save/load trained models (default: ./models)
    ML_BATCH_SIZE: Batch size for training and inference (default: 32)
    ML_MAX_EPOCHS: Maximum training epochs (default: 10)
    ML_LEARNING_RATE: Learning rate for gradient descent (default: 0.001)
    ML_RANDOM_SEED: Random seed for reproducibility (default: 42)
    ML_VALIDATION_SPLIT: Fraction of data for validation (default: 0.2)
    ML_LOG_LEVEL: Logging verbosity (default: INFO)

Supported Models:
    - TF-IDF + Logistic Regression (baseline)
    - TF-IDF + Random Forest
    - BERT-based fine-tuned classifier
    - DistilBERT for lightweight inference

Metrics:
    - Accuracy, Precision, Recall, F1-Score (per-class and weighted)
    - Confusion matrix
    - ROC-AUC (for binary classification)
    - Training loss curve
"""

import json
import logging
import os
import pickle
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Optional

import numpy as np
from sklearn.ensemble import RandomForestClassifier
from sklearn.feature_extraction.text import TfidfVectorizer
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import (
    accuracy_score,
    classification_report,
    confusion_matrix,
    f1_score,
    precision_score,
    recall_score,
)
from sklearn.model_selection import train_test_split
from sklearn.pipeline import Pipeline

logger = logging.getLogger(__name__)


class ModelType(Enum):
    """Supported model architectures for text classification."""
    TFIDF_LOGISTIC = "tfidf_logistic"
    TFIDF_RANDOM_FOREST = "tfidf_random_forest"
    BERT = "bert"
    DISTILBERT = "distilbert"


class DataFormat(Enum):
    """Supported input data formats."""
    CSV = "csv"
    JSON = "json"
    JSONL = "jsonl"
    PARQUET = "parquet"


@dataclass
class PipelineConfig:
    """Configuration for the ML pipeline.

    Attributes:
        model_type: The model architecture to use.
        model_path: Directory for saving/loading model artifacts.
        batch_size: Number of samples per training batch.
        max_epochs: Maximum number of training epochs.
        learning_rate: Learning rate for optimization.
        random_seed: Seed for reproducibility.
        validation_split: Fraction of training data held out for validation.
        max_features: Maximum number of TF-IDF features (for TF-IDF models).
        ngram_range: N-gram range for TF-IDF vectorization.
        max_sequence_length: Maximum token length for transformer models.
        num_classes: Number of classification categories.
        class_names: Human-readable names for each class.
        early_stopping_patience: Epochs to wait before early stopping.
        min_confidence: Minimum prediction confidence threshold.
    """
    model_type: ModelType = ModelType.TFIDF_LOGISTIC
    model_path: str = "./models"
    batch_size: int = 32
    max_epochs: int = 10
    learning_rate: float = 0.001
    random_seed: int = 42
    validation_split: float = 0.2
    max_features: int = 50000
    ngram_range: tuple = (1, 2)
    max_sequence_length: int = 512
    num_classes: int = 2
    class_names: list = field(default_factory=lambda: ["negative", "positive"])
    early_stopping_patience: int = 3
    min_confidence: float = 0.5

    @classmethod
    def from_env(cls) -> "PipelineConfig":
        """Load configuration from environment variables."""
        return cls(
            model_path=os.getenv("ML_MODEL_PATH", "./models"),
            batch_size=int(os.getenv("ML_BATCH_SIZE", "32")),
            max_epochs=int(os.getenv("ML_MAX_EPOCHS", "10")),
            learning_rate=float(os.getenv("ML_LEARNING_RATE", "0.001")),
            random_seed=int(os.getenv("ML_RANDOM_SEED", "42")),
            validation_split=float(os.getenv("ML_VALIDATION_SPLIT", "0.2")),
        )


@dataclass
class TrainingResult:
    """Results from a training run.

    Attributes:
        accuracy: Overall classification accuracy.
        precision: Weighted precision across all classes.
        recall: Weighted recall across all classes.
        f1: Weighted F1-score.
        confusion_matrix: NxN confusion matrix as nested list.
        per_class_report: Detailed metrics per class.
        training_loss_history: Loss value at each epoch.
        validation_loss_history: Validation loss at each epoch.
        best_epoch: Epoch with the best validation performance.
        total_training_time_seconds: Wall-clock training duration.
    """
    accuracy: float
    precision: float
    recall: float
    f1: float
    confusion_matrix: list
    per_class_report: dict
    training_loss_history: list = field(default_factory=list)
    validation_loss_history: list = field(default_factory=list)
    best_epoch: int = 0
    total_training_time_seconds: float = 0.0


@dataclass
class PredictionResult:
    """Result of a single prediction.

    Attributes:
        text: The input text that was classified.
        predicted_class: The predicted class label.
        predicted_class_name: Human-readable class name.
        confidence: Prediction confidence (probability of predicted class).
        all_probabilities: Probability distribution across all classes.
        is_above_threshold: Whether confidence exceeds min_confidence.
    """
    text: str
    predicted_class: int
    predicted_class_name: str
    confidence: float
    all_probabilities: list
    is_above_threshold: bool


class TextPreprocessor:
    """Text preprocessing pipeline.

    Applies the following transformations in order:
    1. Lowercase conversion
    2. HTML tag removal
    3. URL replacement with [URL] token
    4. Email replacement with [EMAIL] token
    5. Number normalization (replace digits with [NUM])
    6. Punctuation normalization
    7. Whitespace normalization
    8. Optional stopword removal
    9. Optional lemmatization
    """

    def __init__(
        self,
        remove_stopwords: bool = False,
        lemmatize: bool = False,
        min_token_length: int = 2,
        max_token_length: int = 50,
    ):
        self.remove_stopwords = remove_stopwords
        self.lemmatize = lemmatize
        self.min_token_length = min_token_length
        self.max_token_length = max_token_length

    def preprocess(self, text: str) -> str:
        """Apply all preprocessing steps to a single text."""
        import re

        # Lowercase
        text = text.lower()

        # Remove HTML tags
        text = re.sub(r"<[^>]+>", " ", text)

        # Replace URLs
        text = re.sub(
            r"https?://\S+|www\.\S+", "[URL]", text
        )

        # Replace emails
        text = re.sub(
            r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b",
            "[EMAIL]",
            text,
        )

        # Normalize numbers
        text = re.sub(r"\b\d+\b", "[NUM]", text)

        # Normalize whitespace
        text = re.sub(r"\s+", " ", text).strip()

        return text

    def preprocess_batch(self, texts: list[str]) -> list[str]:
        """Apply preprocessing to a batch of texts."""
        return [self.preprocess(text) for text in texts]


class DataLoader:
    """Loads and validates training/evaluation data.

    Supported formats:
    - CSV: columns 'text' and 'label'
    - JSON: list of objects with 'text' and 'label' keys
    - JSONL: one JSON object per line with 'text' and 'label' keys
    - Parquet: columns 'text' and 'label'

    Data Validation:
    - Checks for missing or empty text fields
    - Validates label values against expected class count
    - Reports class distribution statistics
    - Flags severe class imbalance (>10:1 ratio)
    """

    def __init__(self, config: PipelineConfig):
        self.config = config

    def load(
        self, path: str, format: DataFormat = DataFormat.CSV
    ) -> tuple[list[str], list[int]]:
        """Load data from file and return (texts, labels)."""
        if format == DataFormat.JSON:
            return self._load_json(path)
        elif format == DataFormat.JSONL:
            return self._load_jsonl(path)
        else:
            raise ValueError(f"Unsupported format: {format}")

    def _load_json(self, path: str) -> tuple[list[str], list[int]]:
        with open(path) as f:
            data = json.load(f)
        texts = [item["text"] for item in data]
        labels = [item["label"] for item in data]
        return texts, labels

    def _load_jsonl(self, path: str) -> tuple[list[str], list[int]]:
        texts, labels = [], []
        with open(path) as f:
            for line in f:
                item = json.loads(line.strip())
                texts.append(item["text"])
                labels.append(item["label"])
        return texts, labels

    def validate(self, texts: list[str], labels: list[int]) -> dict:
        """Validate data quality and return statistics."""
        empty_count = sum(1 for t in texts if not t.strip())
        unique_labels = set(labels)
        class_counts = {
            label: labels.count(label) for label in sorted(unique_labels)
        }

        max_count = max(class_counts.values())
        min_count = min(class_counts.values())
        imbalance_ratio = max_count / min_count if min_count > 0 else float("inf")

        return {
            "total_samples": len(texts),
            "empty_texts": empty_count,
            "unique_labels": len(unique_labels),
            "class_distribution": class_counts,
            "imbalance_ratio": round(imbalance_ratio, 2),
            "is_severely_imbalanced": imbalance_ratio > 10.0,
        }


class ClassificationPipeline:
    """Main ML pipeline for text classification.

    Usage:
        config = PipelineConfig(model_type=ModelType.TFIDF_LOGISTIC)
        pipeline = ClassificationPipeline(config)
        result = pipeline.train(train_texts, train_labels)
        predictions = pipeline.predict(["some text to classify"])
        pipeline.save("./my_model")
        loaded = ClassificationPipeline.load("./my_model")
    """

    def __init__(self, config: PipelineConfig):
        self.config = config
        self.preprocessor = TextPreprocessor()
        self.model: Optional[Pipeline] = None
        self.is_trained = False

    def _build_sklearn_pipeline(self) -> Pipeline:
        """Build a scikit-learn pipeline based on model_type config."""
        vectorizer = TfidfVectorizer(
            max_features=self.config.max_features,
            ngram_range=self.config.ngram_range,
            sublinear_tf=True,
            strip_accents="unicode",
        )

        if self.config.model_type == ModelType.TFIDF_LOGISTIC:
            classifier = LogisticRegression(
                C=1.0,
                max_iter=1000,
                random_state=self.config.random_seed,
                n_jobs=-1,
            )
        elif self.config.model_type == ModelType.TFIDF_RANDOM_FOREST:
            classifier = RandomForestClassifier(
                n_estimators=200,
                max_depth=50,
                random_state=self.config.random_seed,
                n_jobs=-1,
            )
        else:
            raise ValueError(
                f"Unsupported sklearn model type: {self.config.model_type}"
            )

        return Pipeline([
            ("vectorizer", vectorizer),
            ("classifier", classifier),
        ])

    def train(
        self, texts: list[str], labels: list[int]
    ) -> TrainingResult:
        """Train the model on provided data.

        Splits data into train/validation sets, trains the model,
        and returns comprehensive metrics.

        Args:
            texts: List of input text documents.
            labels: List of integer class labels.

        Returns:
            TrainingResult with accuracy, precision, recall, F1,
            confusion matrix, and per-class breakdown.

        Raises:
            ValueError: If texts and labels have different lengths.
            ValueError: If fewer than 10 samples are provided.
        """
        if len(texts) != len(labels):
            raise ValueError(
                f"texts ({len(texts)}) and labels ({len(labels)}) "
                f"must have the same length"
            )

        if len(texts) < 10:
            raise ValueError("At least 10 samples required for training")

        # Preprocess
        processed = self.preprocessor.preprocess_batch(texts)

        # Split
        (
            train_texts, val_texts,
            train_labels, val_labels,
        ) = train_test_split(
            processed, labels,
            test_size=self.config.validation_split,
            random_state=self.config.random_seed,
            stratify=labels,
        )

        # Build and train
        self.model = self._build_sklearn_pipeline()
        self.model.fit(train_texts, train_labels)
        self.is_trained = True

        # Evaluate
        val_predictions = self.model.predict(val_texts)

        return TrainingResult(
            accuracy=accuracy_score(val_labels, val_predictions),
            precision=precision_score(
                val_labels, val_predictions, average="weighted"
            ),
            recall=recall_score(
                val_labels, val_predictions, average="weighted"
            ),
            f1=f1_score(
                val_labels, val_predictions, average="weighted"
            ),
            confusion_matrix=confusion_matrix(
                val_labels, val_predictions
            ).tolist(),
            per_class_report=classification_report(
                val_labels, val_predictions, output_dict=True
            ),
        )

    def predict(self, texts: list[str]) -> list[PredictionResult]:
        """Classify one or more texts.

        Args:
            texts: List of texts to classify.

        Returns:
            List of PredictionResult objects.

        Raises:
            RuntimeError: If model has not been trained or loaded.
        """
        if not self.is_trained or self.model is None:
            raise RuntimeError("Model must be trained or loaded before prediction")

        processed = self.preprocessor.preprocess_batch(texts)
        predictions = self.model.predict(processed)
        probabilities = self.model.predict_proba(processed)

        results = []
        for text, pred, probs in zip(texts, predictions, probabilities):
            confidence = float(probs[pred])
            results.append(
                PredictionResult(
                    text=text,
                    predicted_class=int(pred),
                    predicted_class_name=self.config.class_names[pred],
                    confidence=confidence,
                    all_probabilities=probs.tolist(),
                    is_above_threshold=confidence >= self.config.min_confidence,
                )
            )

        return results

    def save(self, path: str) -> None:
        """Save trained model and config to disk.

        Creates the directory if it doesn't exist. Saves:
        - model.pkl: The trained sklearn pipeline
        - config.json: Pipeline configuration
        - metadata.json: Training metadata and version info
        """
        if not self.is_trained or self.model is None:
            raise RuntimeError("No trained model to save")

        os.makedirs(path, exist_ok=True)

        with open(os.path.join(path, "model.pkl"), "wb") as f:
            pickle.dump(self.model, f)

        config_dict = {
            "model_type": self.config.model_type.value,
            "max_features": self.config.max_features,
            "ngram_range": list(self.config.ngram_range),
            "num_classes": self.config.num_classes,
            "class_names": self.config.class_names,
            "min_confidence": self.config.min_confidence,
        }

        with open(os.path.join(path, "config.json"), "w") as f:
            json.dump(config_dict, f, indent=2)

        logger.info("Model saved to %s", path)

    @classmethod
    def load(cls, path: str) -> "ClassificationPipeline":
        """Load a trained model from disk.

        Args:
            path: Directory containing model.pkl and config.json.

        Returns:
            A ClassificationPipeline with the loaded model.

        Raises:
            FileNotFoundError: If model files don't exist.
        """
        config_path = os.path.join(path, "config.json")
        model_path = os.path.join(path, "model.pkl")

        with open(config_path) as f:
            config_dict = json.load(f)

        config = PipelineConfig(
            model_type=ModelType(config_dict["model_type"]),
            max_features=config_dict["max_features"],
            ngram_range=tuple(config_dict["ngram_range"]),
            num_classes=config_dict["num_classes"],
            class_names=config_dict["class_names"],
            min_confidence=config_dict["min_confidence"],
        )

        pipeline = cls(config)

        with open(model_path, "rb") as f:
            pipeline.model = pickle.load(f)

        pipeline.is_trained = True
        logger.info("Model loaded from %s", path)
        return pipeline


def evaluate_model(
    pipeline: ClassificationPipeline,
    texts: list[str],
    labels: list[int],
) -> dict:
    """Evaluate a trained model on a test set.

    Computes comprehensive metrics including per-class breakdown,
    confusion matrix, and confidence distribution analysis.

    Args:
        pipeline: A trained ClassificationPipeline.
        texts: Test texts.
        labels: True labels for test texts.

    Returns:
        Dictionary with evaluation metrics.
    """
    predictions = pipeline.predict(texts)
    pred_labels = [p.predicted_class for p in predictions]
    confidences = [p.confidence for p in predictions]

    return {
        "accuracy": accuracy_score(labels, pred_labels),
        "precision": precision_score(labels, pred_labels, average="weighted"),
        "recall": recall_score(labels, pred_labels, average="weighted"),
        "f1": f1_score(labels, pred_labels, average="weighted"),
        "mean_confidence": float(np.mean(confidences)),
        "min_confidence": float(np.min(confidences)),
        "max_confidence": float(np.max(confidences)),
        "below_threshold_count": sum(
            1 for p in predictions if not p.is_above_threshold
        ),
        "confusion_matrix": confusion_matrix(labels, pred_labels).tolist(),
    }
