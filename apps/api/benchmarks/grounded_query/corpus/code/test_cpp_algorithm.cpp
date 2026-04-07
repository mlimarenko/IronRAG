#include <vector>
#include <algorithm>
#include <stdexcept>
#include <functional>
#include <string>

/**
 * A generic sorted collection that maintains elements in order
 * and provides efficient search and statistical operations.
 */
template <typename T>
class SortedCollection {
public:
    SortedCollection() = default;

    explicit SortedCollection(std::vector<T> initial) : data_(std::move(initial)) {
        std::sort(data_.begin(), data_.end());
    }

    /**
     * Inserts an element into the collection while maintaining sorted order.
     * Uses binary search to find the correct insertion position,
     * resulting in O(log n) search + O(n) shift.
     *
     * @param value The element to insert.
     */
    void insert(const T& value) {
        auto pos = std::lower_bound(data_.begin(), data_.end(), value);
        data_.insert(pos, value);
    }

    /**
     * Performs binary search for the given value.
     * Returns the index of the element if found, or -1 if not present.
     * Time complexity: O(log n).
     *
     * @param target The value to search for.
     * @return Index of the element, or -1 if not found.
     */
    int binarySearch(const T& target) const {
        int low = 0;
        int high = static_cast<int>(data_.size()) - 1;

        while (low <= high) {
            int mid = low + (high - low) / 2;

            if (data_[mid] == target) {
                return mid;
            } else if (data_[mid] < target) {
                low = mid + 1;
            } else {
                high = mid - 1;
            }
        }

        return -1;
    }

    /**
     * Sorts the collection using merge sort algorithm.
     * This is a stable sort with guaranteed O(n log n) time complexity.
     * Useful when stability is required over the default introsort.
     */
    void mergeSort() {
        if (data_.size() <= 1) return;
        data_ = mergeSortImpl(data_);
    }

    /**
     * Computes the median value of the collection.
     * For even-sized collections, returns the lower median.
     *
     * @return The median element.
     * @throws std::runtime_error if the collection is empty.
     */
    T median() const {
        if (data_.empty()) {
            throw std::runtime_error("Cannot compute median of empty collection");
        }

        size_t mid = data_.size() / 2;
        if (data_.size() % 2 == 0) {
            return data_[mid - 1];
        }
        return data_[mid];
    }

    size_t size() const { return data_.size(); }
    bool empty() const { return data_.empty(); }
    const T& at(size_t index) const { return data_.at(index); }

private:
    std::vector<T> data_;

    std::vector<T> mergeSortImpl(const std::vector<T>& arr) {
        if (arr.size() <= 1) return arr;

        size_t mid = arr.size() / 2;
        std::vector<T> left(arr.begin(), arr.begin() + mid);
        std::vector<T> right(arr.begin() + mid, arr.end());

        left = mergeSortImpl(left);
        right = mergeSortImpl(right);

        return merge(left, right);
    }

    std::vector<T> merge(const std::vector<T>& left, const std::vector<T>& right) {
        std::vector<T> result;
        result.reserve(left.size() + right.size());

        size_t i = 0, j = 0;
        while (i < left.size() && j < right.size()) {
            if (left[i] <= right[j]) {
                result.push_back(left[i++]);
            } else {
                result.push_back(right[j++]);
            }
        }

        while (i < left.size()) result.push_back(left[i++]);
        while (j < right.size()) result.push_back(right[j++]);

        return result;
    }
};
