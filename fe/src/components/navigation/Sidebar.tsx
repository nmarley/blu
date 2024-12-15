import { FolderOpen, Tags, Search, Key, Settings } from "lucide-react";
import { cn } from "@/lib/utils";

const navigation = [
  { name: "Files", icon: FolderOpen },
  { name: "Tags", icon: Tags },
  { name: "Search", icon: Search },
  { name: "Keys", icon: Key },
  { name: "Settings", icon: Settings },
];

interface SidebarProps {
  currentSection: string;
  onNavigate: (section: string) => void;
}

export function Sidebar({ currentSection, onNavigate }: SidebarProps) {
  return (
    <div className="pb-12 w-64">
      <div className="space-y-4 py-4">
        <div className="px-3 py-2">
          <div className="space-y-1">
            {navigation.map((item) => (
              <button
                key={item.name}
                onClick={() => onNavigate(item.name)}
                className={cn(
                  "w-full flex items-center gap-3 rounded-lg px-3 py-2 text-sm transition-colors",
                  currentSection === item.name
                    ? "bg-secondary text-secondary-foreground"
                    : "hover:bg-secondary/80"
                )}
              >
                <item.icon className="h-4 w-4" />
                {item.name}
              </button>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}