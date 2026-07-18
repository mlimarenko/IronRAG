export const TABLE_PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const
export type TablePageSizeOption = (typeof TABLE_PAGE_SIZE_OPTIONS)[number]
