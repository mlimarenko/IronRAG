defmodule RateLimiter do
  @moduledoc """
  A GenServer-based rate limiter using the token bucket algorithm.
  Each bucket allows a configurable number of requests per time window.
  Tokens are replenished automatically at a fixed interval.
  """

  use GenServer

  defstruct [
    :max_tokens,
    :refill_rate,
    :refill_interval,
    buckets: %{}
  ]

  @type bucket :: %{
    tokens: non_neg_integer(),
    last_refill: integer()
  }

  # Client API

  @doc """
  Starts the rate limiter process with the given options.

  ## Options
    * `:max_tokens` - Maximum tokens per bucket (default: 100)
    * `:refill_rate` - Tokens added per refill cycle (default: 10)
    * `:refill_interval` - Milliseconds between refills (default: 1000)
  """
  def start_link(opts \\ []) do
    name = Keyword.get(opts, :name, __MODULE__)
    GenServer.start_link(__MODULE__, opts, name: name)
  end

  @doc """
  Checks whether a request is allowed for the given client key.
  Consumes one token if allowed. Returns `{:ok, remaining}` on success
  or `{:error, :rate_limited, retry_after_ms}` if the bucket is empty.
  """
  def check_rate(server \\ __MODULE__, client_key) do
    GenServer.call(server, {:check_rate, client_key})
  end

  @doc """
  Returns the current token count and bucket state for a given client.
  Useful for including rate limit headers in HTTP responses.
  """
  def get_bucket_info(server \\ __MODULE__, client_key) do
    GenServer.call(server, {:get_info, client_key})
  end

  @doc """
  Resets the bucket for a specific client, restoring it to full capacity.
  Typically used by admin endpoints or after a ban is lifted.
  """
  def reset_bucket(server \\ __MODULE__, client_key) do
    GenServer.cast(server, {:reset, client_key})
  end

  # Server Callbacks

  @impl true
  def init(opts) do
    state = %__MODULE__{
      max_tokens: Keyword.get(opts, :max_tokens, 100),
      refill_rate: Keyword.get(opts, :refill_rate, 10),
      refill_interval: Keyword.get(opts, :refill_interval, 1_000)
    }

    schedule_refill(state.refill_interval)
    {:ok, state}
  end

  @impl true
  def handle_call({:check_rate, client_key}, _from, state) do
    bucket = get_or_create_bucket(state, client_key)

    if bucket.tokens > 0 do
      updated = %{bucket | tokens: bucket.tokens - 1}
      new_state = put_bucket(state, client_key, updated)
      {:reply, {:ok, updated.tokens}, new_state}
    else
      retry_after = state.refill_interval
      {:reply, {:error, :rate_limited, retry_after}, state}
    end
  end

  @impl true
  def handle_call({:get_info, client_key}, _from, state) do
    bucket = get_or_create_bucket(state, client_key)

    info = %{
      tokens: bucket.tokens,
      max_tokens: state.max_tokens,
      refill_rate: state.refill_rate
    }

    {:reply, {:ok, info}, state}
  end

  @impl true
  def handle_cast({:reset, client_key}, state) do
    bucket = %{tokens: state.max_tokens, last_refill: now_ms()}
    {:noreply, put_bucket(state, client_key, bucket)}
  end

  @impl true
  def handle_info(:refill, state) do
    new_buckets =
      Map.new(state.buckets, fn {key, bucket} ->
        new_tokens = min(bucket.tokens + state.refill_rate, state.max_tokens)
        {key, %{bucket | tokens: new_tokens, last_refill: now_ms()}}
      end)

    schedule_refill(state.refill_interval)
    {:noreply, %{state | buckets: new_buckets}}
  end

  # Private Helpers

  defp get_or_create_bucket(state, key) do
    Map.get(state.buckets, key, %{tokens: state.max_tokens, last_refill: now_ms()})
  end

  defp put_bucket(state, key, bucket) do
    %{state | buckets: Map.put(state.buckets, key, bucket)}
  end

  defp schedule_refill(interval) do
    Process.send_after(self(), :refill, interval)
  end

  defp now_ms, do: System.monotonic_time(:millisecond)
end
