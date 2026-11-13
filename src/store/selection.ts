import { create } from "zustand";

/** 表格选中行，键为 `host:port`；测速按钮据此决定是整池还是选中行。 */
export type SelectionStore = {
  ids: string[];
  setIds: (ids: string[]) => void;
  toggle: (id: string) => void;
  clear: () => void;
};

export const useSelectionStore = create<SelectionStore>()((set) => ({
  ids: [],
  setIds: (ids) => set({ ids }),
  toggle: (id) =>
    set((state) => ({
      ids: state.ids.includes(id)
        ? state.ids.filter((item) => item !== id)
        : [...state.ids, id],
    })),
  clear: () => set({ ids: [] }),
}));
