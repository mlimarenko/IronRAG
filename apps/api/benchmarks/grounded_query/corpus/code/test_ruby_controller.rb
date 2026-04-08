# frozen_string_literal: true

# Controller for managing articles in a content management system.
# Supports CRUD operations with pagination and search.
class ArticlesController < ApplicationController
  before_action :set_article, only: %i[show update destroy]
  before_action :authenticate_user!, except: %i[index show]

  # Lists all published articles with optional search and pagination.
  # Supports filtering by tag and searching by title or body content.
  #
  # @param tag [String] optional tag filter
  # @param q [String] optional search query
  # @param page [Integer] page number (default 1)
  # @return [JSON] paginated list of articles
  def index
    articles = Article.published.order(published_at: :desc)

    if params[:tag].present?
      articles = articles.joins(:tags).where(tags: { name: params[:tag] })
    end

    if params[:q].present?
      search_term = "%#{params[:q].downcase}%"
      articles = articles.where(
        "LOWER(title) LIKE :q OR LOWER(body) LIKE :q",
        q: search_term
      )
    end

    @articles = articles.page(params[:page]).per(25)

    render json: {
      articles: @articles.as_json(include: :tags),
      meta: {
        current_page: @articles.current_page,
        total_pages: @articles.total_pages,
        total_count: @articles.total_count
      }
    }
  end

  # Returns a single article by its slug or ID.
  # Increments the view counter for analytics tracking.
  #
  # @return [JSON] the article with its tags and author
  def show
    @article.increment!(:view_count)

    render json: @article.as_json(
      include: %i[tags author comments],
      methods: :reading_time
    )
  end

  # Creates a new article owned by the current user.
  # Accepts title, body, tag_ids, and optional published_at date.
  #
  # @return [JSON] the created article or validation errors
  def create
    @article = current_user.articles.build(article_params)

    if @article.save
      render json: @article, status: :created
    else
      render json: { errors: @article.errors.full_messages },
             status: :unprocessable_entity
    end
  end

  # Updates an existing article. Only the article owner or an admin
  # can perform this action.
  #
  # @return [JSON] the updated article or validation errors
  def update
    unless current_user.admin? || @article.author == current_user
      return render json: { error: "Not authorized" }, status: :forbidden
    end

    if @article.update(article_params)
      render json: @article
    else
      render json: { errors: @article.errors.full_messages },
             status: :unprocessable_entity
    end
  end

  # Soft-deletes an article by marking it as archived.
  # Only admins can permanently destroy articles.
  #
  # @return [JSON] confirmation message
  def destroy
    if current_user.admin?
      @article.destroy
      render json: { message: "Article permanently deleted" }
    else
      @article.update(archived: true)
      render json: { message: "Article archived" }
    end
  end

  private

  def set_article
    @article = Article.find_by!(slug: params[:id])
  rescue ActiveRecord::RecordNotFound
    render json: { error: "Article not found" }, status: :not_found
  end

  def article_params
    params.require(:article).permit(:title, :body, :published_at, tag_ids: [])
  end
end
