import { LibrariesTab } from './LibrariesTab';

/** `/admin/libraries` — global cross-workspace library catalog. Rows link into
 *  the Library Hub (`/admin/library/:id`). */
export default function AdminLibrariesPage() {
  return (
    <div className="flex flex-1 min-h-0 flex-col overflow-hidden p-0">
      <LibrariesTab active />
    </div>
  );
}
