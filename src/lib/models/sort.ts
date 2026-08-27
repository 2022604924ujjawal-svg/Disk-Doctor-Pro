export const SORT_DIRECTIONS = {
  ascending: 'ascending',
  descending: 'descending',
} as const;

export type SortDirection = (typeof SORT_DIRECTIONS)[keyof typeof SORT_DIRECTIONS];
