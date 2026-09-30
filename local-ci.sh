#!/usr/bin/env bash
set -euo pipefail

# Couleurs pour l'affichage
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # Pas de couleur

echo -e "${BLUE}=== [Aramaki] Lancement du pipeline de CI locale ===${NC}"

# Auto-formatage du code
cargo fmt --all 2>/dev/null || true

# 1. Vérification de la syntaxe et compilation rapide
echo -e "\n${YELLOW}[1/4] Vérification de la compilation (cargo check)...${NC}"
if ! cargo check; then
    echo -e "${RED}✘ Échec de la vérification du code !${NC}"
    exit 1
fi
echo -e "${GREEN}✔ Synthaxe et types validés.${NC}"

# 2. Analyse statique (Clippy)
echo -e "\n${YELLOW}[2/4] Analyse statique (cargo clippy)...${NC}"
if ! cargo clippy -- -D warnings; then
    echo -e "${RED}✘ Clippy a trouvé des avertissements ou erreurs !${NC}"
    exit 1
fi
echo -e "${GREEN}✔ Analyse statique validée.${NC}"

# 3. Compilation Release
echo -e "\n${YELLOW}[3/4] Compilation du binaire Release (cargo build --release)...${NC}"
if ! cargo build --release; then
    echo -e "${RED}✘ Échec de la compilation Release !${NC}"
    exit 1
fi
echo -e "${GREEN}✔ Binaire Release compilé avec succès.${NC}"

# 4. Vérification de la construction Docker locale (si Docker est disponible)
if command -v docker &> /dev/null; then
    echo -e "\n${YELLOW}[4/4] Test de build Docker local...${NC}"
    if docker build -t amaraki:local .; then
        echo -e "${GREEN}✔ Image Docker construite avec succès.${NC}"
    else
        echo -e "${RED}✘ Échec de la construction Docker locale !${NC}"
        exit 1
    fi
else
    echo -e "\n${YELLOW}[4/4] Docker non disponible, étape ignorée.${NC}"
fi

echo -e "\n${GREEN}===========================================${NC}"
echo -e "${GREEN}  ✓ TOUTES LES VÉRIFICATIONS SONT OK !      ${NC}"
echo -e "${GREEN}===========================================${NC}"
