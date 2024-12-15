import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { KeyStatus } from '@/types';
import { Lock, Plus, Search, Settings, Unlock } from 'lucide-react';

interface HeaderProps {
    keyStatus: KeyStatus;
    onSearch: (query: string) => void;
    onAddFiles: () => void;
    onOpenSettings: () => void;
}

export function Header({
    keyStatus,
    onSearch,
    onAddFiles,
    onOpenSettings,
}: HeaderProps) {
    return (
        <header className="border-b border-border bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
            <div className="container flex h-14 items-center px-4">
                <div className="flex items-center space-x-2 mr-4">
                    <span className="font-bold text-xl">blu</span>
                    {keyStatus.loaded ? (
                        <Unlock className="h-4 w-4 text-green-500" />
                    ) : (
                        <Lock className="h-4 w-4 text-yellow-500" />
                    )}
                </div>

                <div className="flex-1 flex items-center space-x-4">
                    <div className="w-full max-w-lg">
                        <div className="relative">
                            <Search className="absolute left-2 top-2.5 h-4 w-4 text-muted-foreground" />
                            <Input
                                placeholder="Search content, paths, tags..."
                                className="pl-8"
                                onChange={(e) => onSearch(e.target.value)}
                            />
                        </div>
                    </div>

                    <Button onClick={onAddFiles} variant="default">
                        <Plus className="h-4 w-4 mr-2" />
                        Add Files
                    </Button>

                    <Button
                        variant="ghost"
                        size="icon"
                        onClick={onOpenSettings}
                    >
                        <Settings className="h-4 w-4" />
                    </Button>
                </div>
            </div>
        </header>
    );
}
